use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::{
    stream::BoxStream,
    task::{noop_waker_ref, Context, Poll},
    Future, StreamExt,
};
use tokio::{
    sync::{broadcast, mpsc},
    task::JoinHandle,
};

use block_albatross::{Block, BlockType, SignedTendermintProposal, ViewChange, ViewChangeProof};
use blockchain_albatross::{AbstractBlockchain, BlockchainEvent, ForkEvent, PushResult};
use bls::CompressedPublicKey;
use consensus_albatross::{
    sync::block_queue::BlockTopic, Consensus, ConsensusEvent, ConsensusProxy,
};
use database::{Database, Environment, ReadTransaction, WriteTransaction};
use hash::Blake2bHash;
use network_interface::network::{Network, Topic};
use nimiq_block_production_albatross::BlockProducer;
use nimiq_tendermint::TendermintReturn;
use nimiq_validator_network::ValidatorNetwork;

use crate::micro::{ProduceMicroBlock, ProduceMicroBlockEvent};
use crate::r#macro::{PersistedMacroState, ProduceMacroBlock};
use crate::slash::ForkProofPool;

pub struct ProposalTopic;
impl Topic for ProposalTopic {
    type Item = SignedTendermintProposal;

    fn topic(&self) -> String {
        "tendermint-proposal".to_owned()
    }

    fn validate(&self) -> bool {
        false
    }
}

enum ValidatorStakingState {
    Active,
    Parked,
    Inactive,
    NoStake,
}

struct ActiveEpochState {
    validator_id: u16,
}

struct BlockchainState {
    fork_proofs: ForkProofPool,
}

struct ProduceMicroBlockState {
    view_number: u32,
    view_change_proof: Option<ViewChangeProof>,
    view_change: Option<ViewChange>,
}

pub struct Validator<TNetwork: Network, TValidatorNetwork: ValidatorNetwork + 'static> {
    pub consensus: ConsensusProxy<TNetwork>,
    network: Arc<TValidatorNetwork>,
    // TODO: Also have the validator ID here.
    signing_key: bls::KeyPair,
    wallet_key: Option<keys::KeyPair>,
    database: Database,
    env: Environment,

    proposal_task: Option<
        JoinHandle<
            Result<
                BoxStream<'static, (<ProposalTopic as Topic>::Item, TValidatorNetwork::PubsubId)>,
                TValidatorNetwork::Error,
            >,
        >,
    >,

    consensus_event_rx: broadcast::Receiver<ConsensusEvent<TNetwork>>,
    blockchain_event_rx: mpsc::UnboundedReceiver<BlockchainEvent>,
    fork_event_rx: mpsc::UnboundedReceiver<ForkEvent>,

    epoch_state: Option<ActiveEpochState>,
    blockchain_state: BlockchainState,

    macro_producer: Option<ProduceMacroBlock>,
    macro_state: Option<PersistedMacroState<TValidatorNetwork>>,

    micro_producer: Option<ProduceMicroBlock<TValidatorNetwork>>,
    micro_state: ProduceMicroBlockState,
}

impl<TNetwork: Network, TValidatorNetwork: ValidatorNetwork>
    Validator<TNetwork, TValidatorNetwork>
{
    const MACRO_STATE_DB_NAME: &'static str = "ValidatorState";
    const MACRO_STATE_KEY: &'static str = "validatorState";
    const VIEW_CHANGE_DELAY: Duration = Duration::from_secs(10);
    const FORK_PROOFS_MAX_SIZE: usize = 1_000; // bytes

    pub fn new(
        consensus: &Consensus<TNetwork>,
        network: Arc<TValidatorNetwork>,
        signing_key: bls::KeyPair,
        wallet_key: Option<keys::KeyPair>,
    ) -> Self {
        let consensus_event_rx = consensus.subscribe_events();
        let blockchain_event_rx = consensus.blockchain.notifier.write().as_stream();
        let fork_event_rx = consensus.blockchain.fork_notifier.write().as_stream();

        let blockchain_state = BlockchainState {
            fork_proofs: ForkProofPool::new(),
        };

        let micro_state = ProduceMicroBlockState {
            view_number: consensus.blockchain.view_number(),
            view_change_proof: None,
            view_change: None,
        };

        let env = consensus.env.clone();
        let database = env.open_database(Self::MACRO_STATE_DB_NAME.to_string());

        let macro_state: Option<PersistedMacroState<TValidatorNetwork>> = {
            let read_transaction = ReadTransaction::new(&env);
            read_transaction.get(&database, Self::MACRO_STATE_KEY)
        };

        // Spawn into task so the lifetime does not expire.
        // Also start executing immediately as we will need to wait for this on the first macro block.
        let nw = network.clone();
        let proposal_task = Some(tokio::spawn(
            async move { nw.subscribe(&ProposalTopic).await },
        ));

        let mut this = Self {
            consensus: consensus.proxy(),
            network,
            signing_key,
            wallet_key,
            database,
            env,

            proposal_task,

            consensus_event_rx,
            blockchain_event_rx,
            fork_event_rx,

            epoch_state: None,
            blockchain_state,

            macro_producer: None,
            macro_state,

            micro_producer: None,
            micro_state,
        };
        this.init();
        this
    }

    fn init(&mut self) {
        self.init_epoch();
        self.init_block_producer();
    }

    fn init_epoch(&mut self) {
        log::debug!("Initializing epoch");

        let validators = self.consensus.blockchain.current_validators().unwrap();

        // TODO: This code block gets this validators position in the validators struct by searching it
        //  with its public key. This is an insane way of doing this. Just start saving the validator
        //  id in the Validator struct (the one in this crate).
        self.epoch_state = None;
        for (i, validator) in validators.iter().enumerate() {
            if validator.public_key.compressed() == &self.signing_key.public_key.compress() {
                self.epoch_state = Some(ActiveEpochState {
                    validator_id: i as u16,
                });
                break;
            }
        }

        let validator_keys: Vec<CompressedPublicKey> = self
            .consensus
            .blockchain
            .current_validators()
            .unwrap()
            .iter()
            .map(|validator| validator.public_key.compressed().clone())
            .collect();
        let key = self.signing_key.clone();
        let nw = self.network.clone();

        // TODO might better be done without the task.
        // However we have an entire batch to execute the task so it should not be extremely bad.
        // Also the setting up of our own public key record should probably not be done here but in `init` instead.
        tokio::spawn(async move {
            if let Err(err) = nw
                .set_public_key(&key.public_key.compress(), &key.secret_key)
                .await
            {
                error!("could not set up DHT record: {:?}", err);
            }
            nw.set_validators(validator_keys).await;
        });
    }

    fn init_block_producer(&mut self) {
        self.macro_producer = None;
        self.micro_producer = None;

        log::debug!("Initializing block producer");

        if !self.is_active() {
            log::debug!("Validator not active");
            return;
        }

        let _lock = self.consensus.blockchain.lock();
        match self.consensus.blockchain.get_next_block_type(None) {
            BlockType::Macro => {
                let block_producer = BlockProducer::new(
                    self.consensus.blockchain.clone(),
                    self.consensus.mempool.clone(),
                    self.signing_key.clone(),
                );

                let (mut sender, receiver) = mpsc::channel::<(
                    <ProposalTopic as Topic>::Item,
                    TValidatorNetwork::PubsubId,
                )>(2);

                if let Some(task) = self.proposal_task.take() {
                    self.proposal_task = Some(tokio::spawn(async move {
                        if let Ok(stream) = task.await {
                            if let Ok(mut stream) = stream {
                                while let Some(item) = stream.next().await {
                                    let mut cx = Context::from_waker(noop_waker_ref());
                                    match sender.poll_ready(&mut cx) {
                                        Poll::Pending => {
                                            // Todo: buffer proposal if necessary.
                                            log::debug!("Proposal recipient not able to receive new Messages. Waiting to try with the next proposal!");
                                        }
                                        Poll::Ready(Ok(_)) => {
                                            if let Err(_err) = sender.send(item).await {
                                                log::debug!("failed to send message through sender, even though poll_ready returned Ok");
                                            }
                                        }
                                        Poll::Ready(Err(_err)) => {
                                            // recipient is no longer present, leave the loop and return the subscription stream.
                                            log::trace!("Sonder for proposals no longer has a recipient, Block was produced!");
                                            break;
                                        }
                                    }
                                }
                                let mut cx = Context::from_waker(noop_waker_ref());
                                while let Poll::Ready(Some(_)) = stream.poll_next_unpin(&mut cx) {
                                    // noop. Empty out the stream before returning it, dropping all accumulated messages.
                                }
                                Ok(stream)
                            } else {
                                // Todo: Recover from this?
                                panic!("subscription stream returned err");
                            }
                        } else {
                            // Todo: Recover from this?
                            panic!("failed to join subscription task");
                        }
                    }));
                } else {
                    panic!("There is no proposal task. Validator dysfunctional");
                }

                // Take the current state and see if it is applicable to the current height.
                // We do not need to keep it as it is persisted.
                // This will always result in None in case the validator works as intended.
                // Only in case of a crashed node this will result in a value from which Tendermint can resume its work.
                let state = self
                    .macro_state
                    .take()
                    .map(|state| {
                        if state.height == self.consensus.blockchain.block_number() + 1 {
                            Some(state)
                        } else {
                            None
                        }
                    })
                    .flatten();

                self.macro_producer = Some(ProduceMacroBlock::new(
                    self.consensus.blockchain.clone(),
                    self.network.clone(),
                    block_producer,
                    self.signing_key.clone(),
                    self.validator_id(),
                    state,
                    receiver.boxed(),
                ));
            }
            BlockType::Micro => {
                self.micro_state = ProduceMicroBlockState {
                    view_number: self.consensus.blockchain.head().next_view_number(),
                    view_change_proof: None,
                    view_change: None,
                };

                let fork_proofs = self
                    .blockchain_state
                    .fork_proofs
                    .get_fork_proofs_for_block(Self::FORK_PROOFS_MAX_SIZE);
                self.micro_producer = Some(ProduceMicroBlock::new(
                    Arc::clone(&self.consensus.blockchain),
                    Arc::clone(&self.consensus.mempool),
                    Arc::clone(&self.network),
                    self.signing_key.clone(),
                    self.validator_id(),
                    fork_proofs,
                    self.micro_state.view_number,
                    self.micro_state.view_change_proof.clone(),
                    self.micro_state.view_change.clone(),
                    Self::VIEW_CHANGE_DELAY,
                ));
            }
        }
    }

    fn on_blockchain_event(&mut self, event: BlockchainEvent) {
        match event {
            BlockchainEvent::Extended(ref hash) => self.on_blockchain_extended(hash),
            BlockchainEvent::Finalized(ref hash) => self.on_blockchain_extended(hash),
            BlockchainEvent::EpochFinalized(ref hash) => {
                self.on_blockchain_extended(hash);
                self.init_epoch()
            }
            BlockchainEvent::Rebranched(ref old_chain, ref new_chain) => {
                self.on_blockchain_rebranched(old_chain, new_chain)
            }
        }

        self.init_block_producer();
    }

    fn on_blockchain_extended(&mut self, hash: &Blake2bHash) {
        let block = self
            .consensus
            .blockchain
            .get_block(hash, true, None)
            .expect("Head block not found");
        self.blockchain_state.fork_proofs.apply_block(&block);
    }

    fn on_blockchain_rebranched(
        &mut self,
        old_chain: &[(Blake2bHash, Block)],
        new_chain: &[(Blake2bHash, Block)],
    ) {
        for (_hash, block) in old_chain.iter() {
            self.blockchain_state.fork_proofs.revert_block(block);
        }
        for (_hash, block) in new_chain.iter() {
            self.blockchain_state.fork_proofs.apply_block(&block);
        }
    }

    fn on_fork_event(&mut self, event: ForkEvent) {
        match event {
            ForkEvent::Detected(fork_proof) => self.blockchain_state.fork_proofs.insert(fork_proof),
        };
    }

    fn poll_macro(&mut self, cx: &mut Context<'_>) {
        let macro_producer = self.macro_producer.as_mut().unwrap();
        while let Poll::Ready(Some(event)) = macro_producer.poll_next_unpin(cx) {
            match event {
                TendermintReturn::Error(_err) => {}
                TendermintReturn::Result(block) => {
                    // If the event is a result meaning the next macro block was produced we push it onto our local chain
                    let block_copy = block.clone();
                    let result = self
                        .consensus
                        .blockchain
                        .push(Block::Macro(block))
                        .map_err(|e| error!("Failed to push macro block onto the chain: {:?}", e))
                        .ok();
                    if result == Some(PushResult::Extended)
                        || result == Some(PushResult::Rebranched)
                    {
                        if block_copy.is_election_block() {
                            info!(
                                "Publishing Election MacroBlock #{}",
                                &block_copy.header.block_number
                            );
                        } else {
                            info!(
                                "Publishing Checkpoint MacroBlock #{}",
                                &block_copy.header.block_number
                            );
                        }
                        // todo get rid of spawn
                        let nw = self.network.clone();
                        tokio::spawn(async move {
                            trace!("publishing macro block: {:?}", &block_copy);
                            if nw
                                .publish(&BlockTopic, Block::Macro(block_copy))
                                .await
                                .is_err()
                            {
                                error!("Failed to publish Block");
                            }
                        });
                    }
                }
                // in case of a new state update we need to store th enew version of it disregarding any old state which potentially still lingers.
                TendermintReturn::StateUpdate(update) => {
                    let mut write_transaction = WriteTransaction::new(&self.env);
                    let persistable_state = PersistedMacroState::<TValidatorNetwork> {
                        height: self.consensus.blockchain.block_number() + 1,
                        step: update.step.into(),
                        round: update.round,
                        locked_round: update.locked_round,
                        locked_value: update.locked_value,
                        valid_round: update.valid_round,
                        valid_value: update.valid_value,
                    };

                    write_transaction.put::<str, Vec<u8>>(
                        &self.database,
                        Self::MACRO_STATE_KEY,
                        &beserial::Serialize::serialize_to_vec(&persistable_state),
                    );

                    self.macro_state = Some(persistable_state);
                }
            }
        }
    }

    fn poll_micro(&mut self, cx: &mut Context<'_>) {
        let micro_producer = self.micro_producer.as_mut().unwrap();
        while let Poll::Ready(Some(event)) = micro_producer.poll_next_unpin(cx) {
            match event {
                ProduceMicroBlockEvent::MicroBlock(block) => {
                    let block_copy = block.clone();
                    let result = self
                        .consensus
                        .blockchain
                        .push(Block::Micro(block))
                        .map_err(|e| error!("Failed to push our block onto the chain: {:?}", e))
                        .ok();
                    if result == Some(PushResult::Extended)
                        || result == Some(PushResult::Rebranched)
                    {
                        // todo get rid of spawn
                        let nw = self.network.clone();
                        tokio::spawn(async move {
                            trace!("publishing micro block: {:?}", &block_copy);
                            if nw
                                .publish(&BlockTopic, Block::Micro(block_copy))
                                .await
                                .is_err()
                            {
                                error!("Failed to publish Block");
                            }
                        });
                    }
                }
                ProduceMicroBlockEvent::ViewChange(view_change, view_change_proof) => {
                    self.micro_state.view_number = view_change.new_view_number; // needed?
                    self.micro_state.view_change_proof = Some(view_change_proof);
                    self.micro_state.view_change = Some(view_change);
                }
            }
        }
    }

    fn is_active(&self) -> bool {
        self.epoch_state.is_some()
    }

    pub fn validator_id(&self) -> u16 {
        self.epoch_state
            .as_ref()
            .expect("Validator not active")
            .validator_id
    }

    pub fn signing_key(&self) -> bls::KeyPair {
        self.signing_key.clone()
    }
}

impl<TNetwork: Network, TValidatorNetwork: ValidatorNetwork> Future
    for Validator<TNetwork, TValidatorNetwork>
{
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Process consensus updates.
        while let Poll::Ready(Some(event)) = self.consensus_event_rx.poll_next_unpin(cx) {
            match event {
                Ok(ConsensusEvent::Established) => self.init(),
                Err(_) => return Poll::Ready(()),
                _ => {}
            }
        }

        // Process blockchain updates.
        while let Poll::Ready(Some(event)) = self.blockchain_event_rx.poll_next_unpin(cx) {
            if self.consensus.is_established() {
                self.on_blockchain_event(event);
            }
        }

        // Process fork events.
        while let Poll::Ready(Some(event)) = self.fork_event_rx.poll_next_unpin(cx) {
            if self.consensus.is_established() {
                self.on_fork_event(event);
            }
        }

        // If we are an active validator, participate in block production.
        if self.consensus.is_established() && self.is_active() {
            if self.macro_producer.is_some() {
                self.poll_macro(cx);
            }
            if self.micro_producer.is_some() {
                self.poll_micro(cx);
            }
        }

        Poll::Pending
    }
}
