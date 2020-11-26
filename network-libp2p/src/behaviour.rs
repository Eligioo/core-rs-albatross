use std::collections::VecDeque;
use std::sync::Arc;
use std::task::{Poll, Waker, Context};

use libp2p::{
    core::either::{EitherOutput, EitherError},
    swarm::{NetworkBehaviourEventProcess, NetworkBehaviourAction, PollParameters},
    kad::{
        store::MemoryStore,
        handler::{KademliaHandlerIn as KademliaAction},
        Kademlia, KademliaEvent, QueryId,
    },
    NetworkBehaviour,
};
use parking_lot::RwLock;

use nimiq_network_interface::network::NetworkEvent;

use crate::{
    discovery::{
        behaviour::{DiscoveryBehaviour, DiscoveryEvent},
        handler::{HandlerInEvent as DiscoveryAction, HandlerError as DiscoveryError},
        peer_contacts::PeerContactBook,
    },
    message::{
        behaviour::MessageBehaviour,
        handler::{HandlerInEvent as MessageAction, HandlerError as MessageError},
        peer::Peer,
    },
    limit::{
        behaviour::{LimitBehaviour, LimitEvent},
        handler::{HandlerInEvent as LimitAction, HandlerError as LimitError},
    },
    network::Config,
};


pub type NimiqNetworkBehaviourAction = NetworkBehaviourAction<
    EitherOutput<
        EitherOutput<
            EitherOutput<
                DiscoveryAction,
                MessageAction
            >,
            LimitAction
        >,
        KademliaAction<QueryId>,
    >,
    NimiqEvent,
>;

pub type NimiqNetworkBehaviourError = EitherError<
    EitherError<
        EitherError<
            DiscoveryError,
            MessageError
        >,
        LimitError
    >,
    std::io::Error,
>;


#[derive(Debug)]
pub enum NimiqEvent {
    Message(NetworkEvent<Peer>),
    Dht(KademliaEvent),
}

impl From<NetworkEvent<Peer>> for NimiqEvent {
    fn from(event: NetworkEvent<Peer>) -> Self {
        Self::Message(event)
    }
}

impl From<KademliaEvent> for NimiqEvent {
    fn from(event: KademliaEvent) -> Self {
        Self::Dht(event)
    }
}


#[derive(NetworkBehaviour)]
#[behaviour(out_event = "NimiqEvent", poll_method = "poll_event")]
pub struct NimiqBehaviour {
    pub discovery: DiscoveryBehaviour,
    pub message: MessageBehaviour,
    pub limit: LimitBehaviour,
    pub kademlia: Kademlia<MemoryStore>,

    #[behaviour(ignore)]
    events: VecDeque<NimiqEvent>,

    #[behaviour(ignore)]
    waker: Option<Waker>,
}

impl NimiqBehaviour {
    pub fn new(config: Config) -> Self {
        let public_key = config.keypair.public();
        let peer_id = public_key.clone().into_peer_id();

        // TODO: persist to disk
        let peer_contact_book = Arc::new(RwLock::new(PeerContactBook::new(
            Default::default(),
            config.peer_contact.sign(&config.keypair)
        )));
        let discovery = DiscoveryBehaviour::new(config.discovery, config.keypair.clone(), peer_contact_book);

        let message = MessageBehaviour::new(config.message);

        let limit = LimitBehaviour::new(config.limit);

        let store = MemoryStore::new(peer_id.clone());
        let kademlia = Kademlia::with_config(peer_id, store, config.kademlia);

        Self {
            discovery,
            message,
            limit,
            kademlia,
            events: VecDeque::new(),
            waker: None,
        }
    }

    fn poll_event(&mut self, cx: &mut Context, _params: &mut impl PollParameters) -> Poll<NimiqNetworkBehaviourAction> {
        if let Some(event) = self.events.pop_front() {
            log::trace!("NimiqBehaviour: emitting event: {:?}", event);
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(event));
        }

        // Register waker, if we're waiting for an event.
        if self.waker.is_none() {
            self.waker = Some(cx.waker().clone());
        }

        Poll::Pending
    }

    fn emit_event<E>(&mut self, event: E)
        where
            NimiqEvent: From<E>,
    {
        self.events.push_back(event.into());
        self.wake();
    }

    fn wake(&self) {
        if let Some(waker) = &self.waker {
            waker.wake_by_ref();
        }
    }


}

impl NetworkBehaviourEventProcess<DiscoveryEvent> for NimiqBehaviour {
    fn inject_event(&mut self, event: DiscoveryEvent) {
        log::trace!("discovery event: {:?}", event);
    }
}

impl NetworkBehaviourEventProcess<NetworkEvent<Peer>> for NimiqBehaviour {
    fn inject_event(&mut self, event: NetworkEvent<Peer>) {
        log::trace!("NimiqBehaviour::inject_event: {:?}", event);

        /*match event {
            NetworkEvent::PeerJoined(peer) => {
                /*self.limit.peers
                    .insert(peer.id.clone(), Arc::clone(&peer))
                    .map(|p| panic!("Duplicate peer {}", p.id));*/

                self.events.push_back(NetworkEvent::PeerJoined(peer));
            },
            NetworkEvent::PeerLeft(peer) => {
                self.events.push_back(NetworkEvent::PeerLeft(peer));
            },
        }

        self.wake();*/

        self.emit_event(event);
    }
}

impl NetworkBehaviourEventProcess<LimitEvent> for NimiqBehaviour {
    fn inject_event(&mut self, event: LimitEvent) {
        log::trace!("NimiqBehaviour::inject_event: {:?}", event);
    }
}

impl NetworkBehaviourEventProcess<KademliaEvent> for NimiqBehaviour {
    fn inject_event(&mut self, event: KademliaEvent) {
        log::debug!("NimiqBehaviour::inject_event: {:?}", event);
        self.emit_event(event);
    }
}




