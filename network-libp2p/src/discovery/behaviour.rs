use std::{
    collections::{VecDeque, HashSet},
    time::Duration,
    sync::Arc,
};

use libp2p::{
    swarm::{NetworkBehaviour, PollParameters, NetworkBehaviourAction, NotifyHandler},
    core::connection::{ConnectionId, ConnectedPoint},
    identity::Keypair,
    PeerId, Multiaddr,
};
use futures::{
    task::{Context, Poll},
    StreamExt,
};
use rand::{
    seq::IteratorRandom,
    thread_rng,
};
use parking_lot::RwLock;

use nimiq_hash::Blake2bHash;

use super::{
    handler::{DiscoveryHandler, HandlerInEvent, HandlerOutEvent},
    protocol::DiscoveryMessage,
    peer_contacts::{PeerContactBook, Services, Protocols, SignedPeerContact},
};


#[derive(Clone, Debug)]
pub struct DiscoveryConfig {
    /// Genesis hash for the network we want to be connected to.
    pub genesis_hash: Blake2bHash,

    /// Interval in which we want to be updated.
    pub update_interval: Duration,

    /// Minimum update interval, that we will accept. If peer contact updates are received faster than this, they will
    /// be rejected.
    pub min_recv_update_interval: Duration,

    /// How many updated peer contacts we want to receive per update.
    pub update_limit: Option<u16>,

    /// Protocols for which we filter.
    pub protocols_filter: Protocols,

    /// Services for which we filter.
    pub services_filter: Services,

    /// Minimium interval that we will update other peers with.
    pub min_send_update_interval: Duration,
}


#[derive(Clone, Debug)]
pub enum DiscoveryEvent {
    Established {
        peer_id: PeerId,
    },
    Update,
}


pub struct Discovery {
    /// Configuration for the discovery behaviour
    config: DiscoveryConfig,

    /// Identity key pair
    keypair: Keypair,

    /// `PeerId`s of all connected peers.
    connected_peers: HashSet<PeerId>,

    /// Contains all known peer contacts.
    peer_contact_book: Arc<RwLock<PeerContactBook>>,

    /// Queue with events to emit.
    events: VecDeque<NetworkBehaviourAction<HandlerInEvent, DiscoveryEvent>>,
}


impl Discovery {
    // TODO: What interface is needed from the meta behaviour?

    pub fn new(config: DiscoveryConfig, keypair: Keypair, peer_contact_book: Arc<RwLock<PeerContactBook>>) -> Self {
        Self {
            config,
            keypair,
            connected_peers: HashSet::new(),
            peer_contact_book,
            events: VecDeque::new(),
        }
    }

    pub fn peer_contact_book(&self) -> Arc<RwLock<PeerContactBook>> {
        Arc::clone(&self.peer_contact_book)
    }
}


impl NetworkBehaviour for Discovery {
    type ProtocolsHandler = DiscoveryHandler;
    type OutEvent = DiscoveryEvent;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        DiscoveryHandler::new(
            self.config.clone(),
            self.keypair.clone(),
            Arc::clone(&self.peer_contact_book)
        )
    }

    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr> {
        self.peer_contact_book.read()
            .get(peer_id)
            .map(|addresses_opt| addresses_opt.addresses()
                .cloned()
                .collect())
            .unwrap_or_default()
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {
        log::debug!("DiscoveryBehaviour::inject_connected: {}", peer_id);

        self.connected_peers.insert(peer_id.clone());
    }

    fn inject_connection_established(&mut self, peer_id: &PeerId, connection_id: &ConnectionId, connected_point: &ConnectedPoint) {
        log::debug!("DiscoveryBehaviour::inject_connection_established:");
        log::debug!("  - peer_id: {:?}", peer_id);
        log::debug!("  - connection_id: {:?}", connection_id);
        log::debug!("  - connected_point: {:?}", connected_point);

        // TODO: In libp2p 0.29 there is a method for this:
        // connected_point.get_remote_address()
        let remote_address = match connected_point {
            ConnectedPoint::Dialer { address } => address,
            ConnectedPoint::Listener { local_addr, .. } => local_addr,
        };

        self.events.push_back(NetworkBehaviourAction::NotifyHandler {
            peer_id: peer_id.clone(),
            handler: NotifyHandler::One(connection_id.clone()),
            event: HandlerInEvent::ObservedAddress(remote_address.clone())
        });
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        log::debug!("DiscoveryBehaviour::inject_disconnected: {}", peer_id);

        self.connected_peers.remove(peer_id);
    }

    fn inject_event(&mut self, peer_id: PeerId, connection: ConnectionId, event: HandlerOutEvent) {
        log::debug!("DiscoveryBehaviour::inject_event: {}", peer_id);

        match event {
            HandlerOutEvent::PeerExchangeEstablished { peer_contact } => {
                self.events.push_back(NetworkBehaviourAction::GenerateEvent(DiscoveryEvent::Established {
                    peer_id: peer_contact.public_key().clone().into_peer_id(),
                }));
            },
            HandlerOutEvent::ObservedAddresses { observed_addresses } => {
                for address in observed_addresses {
                    self.events.push_back(NetworkBehaviourAction::ReportObservedAddr { address });
                }
            }
            HandlerOutEvent::Update => {
                self.events.push_back(NetworkBehaviourAction::GenerateEvent(DiscoveryEvent::Update))
            },
        }
    }

    fn poll(&mut self, cx: &mut Context, params: &mut impl PollParameters) -> Poll<NetworkBehaviourAction<HandlerInEvent, DiscoveryEvent>> {
        // Emit events
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }

        Poll::Pending
    }
}