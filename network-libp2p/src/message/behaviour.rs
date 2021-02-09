use std::{
    collections::{VecDeque, HashMap},
    sync::Arc,
};

use futures::{
    task::{Context, Poll, Waker},
    channel::mpsc,
};
use libp2p::core::connection::ConnectionId;
use libp2p::core::Multiaddr;
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, NotifyHandler, PollParameters};
use libp2p::{core::ConnectedPoint, PeerId};
use bytes::Bytes;

use nimiq_network_interface::{
    network::NetworkEvent, 
    peer_map::ObservablePeerMap,
    message::MessageType,
};

use super::{
    handler::{HandlerInEvent, HandlerOutEvent, MessageHandler},
    peer::Peer,
};

#[derive(Clone, Debug, Default)]
pub struct MessageConfig {
    // TODO
}

#[derive(Clone, Debug, Default)]
pub struct MessageBehaviour {
    config: MessageConfig,

    events: VecDeque<NetworkBehaviourAction<HandlerInEvent, NetworkEvent<Peer>>>,

    pub(crate) peers: ObservablePeerMap<Peer>,

    message_receivers: HashMap<MessageType, mpsc::Sender<(Bytes, Arc<Peer>)>>,

    waker: Option<Waker>,
}

impl MessageBehaviour {
    pub fn new(config: MessageConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }

    /// Buffers a `NetworkBehaviourAction` that should be emitted by the `poll` method on the next invocation.
    fn push_event(&mut self, event: NetworkBehaviourAction<HandlerInEvent, NetworkEvent<Peer>>) {
        self.events.push_back(event);
        self.wake();
    }

    fn wake(&self) {
        if let Some(waker) = &self.waker {
            waker.wake_by_ref();
        }
    }

    /// Registers a receiver to receive from all peers. This will also make sure that any newly connected peer already
    /// has a receiver (a.k.a. message handler) registered before any messages can be received.
    /// 
    /// # Note
    /// 
    /// When a peer connects, this will be registered in its `MessageDispatch`. Thus you must not register a separate
    /// receiver with the peer.
    /// 
    /// # Arguments
    /// 
    ///  - `type_id`: The message type (e.g. `MessageType::new(200)` for `RequestBlockHashes`)
    ///  - `tx`: The sender through which the data of the messages is sent to the handler.
    /// 
    /// # Panics
    /// 
    /// Panics if a receiver was already registered for this message type.
    /// 
    pub fn receive_from_all(&mut self, type_id: MessageType, tx: mpsc::Sender<(Bytes, Arc<Peer>)>) {
        if self.message_receivers.get(&type_id).is_some() {
            panic!("A receiver for message type {} is already registered", type_id);
        }
        self.message_receivers.insert(type_id, tx);
    }
}

impl NetworkBehaviour for MessageBehaviour {
    type ProtocolsHandler = MessageHandler;
    type OutEvent = NetworkEvent<Peer>;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        MessageHandler::new(self.config.clone())
    }

    fn addresses_of_peer(&mut self, _peer_id: &PeerId) -> Vec<Multiaddr> {
        vec![]
    }

    fn inject_connected(&mut self, _peer_id: &PeerId) {}

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        // No handler exists anymore.
        log::trace!("inject_disconnected: {:?}", peer_id);
    }

    fn inject_connection_established(&mut self, peer_id: &PeerId, connection_id: &ConnectionId, connected_point: &ConnectedPoint) {
        log::info!(
            "Connection established: peer_id={:?}, connection_id={:?}, connected_point={:?}",
            peer_id,
            connection_id,
            connected_point
        );

        // Send an event to the handler that tells it if this is an inbound or outbound connection, and the registered
        // messages handlers, that receive from all peers.
        self.events.push_back(NetworkBehaviourAction::NotifyHandler {
            peer_id: peer_id.clone(),
            handler: NotifyHandler::All, // Really doesn't matter, since we limit the number of connections per PeerId to 1.
            event: HandlerInEvent::PeerConnected {
                peer_id: peer_id.clone(),
                outbound: connected_point.is_dialer(),
                receive_from_all: self.message_receivers.clone(),
            },
        });
    }

    fn inject_connection_closed(&mut self, peer_id: &PeerId, connection_id: &ConnectionId, connected_point: &ConnectedPoint) {
        log::info!(
            "Connection closed: peer_id={:?}, connection_id={:?}, connected_point={:?}",
            peer_id,
            connection_id,
            connected_point
        );

        // If we still know this peer, remove it and emit an `PeerLeft` event to the swarm.
        if let Some(peer) = self.peers.remove(peer_id) {
            log::debug!("Peer disconnected: {:?}", peer);
            self.push_event(NetworkBehaviourAction::GenerateEvent(NetworkEvent::PeerLeft(peer)));
        }
    }

    fn inject_event(&mut self, peer_id: PeerId, _connection: ConnectionId, event: HandlerOutEvent) {
        log::trace!("MessageBehaviour::inject_event: peer_id={:?}: {:?}", peer_id, event);
        match event {
            HandlerOutEvent::PeerJoined { peer } => {
                self.peers.insert(Arc::clone(&peer));
                self.push_event(NetworkBehaviourAction::GenerateEvent(NetworkEvent::PeerJoined(peer)));
            }
            HandlerOutEvent::PeerClosed { peer, reason } => {
                log::debug!("Peer closed: {:?}, reason={:?}", peer, reason);
                self.peers.remove(&peer_id);
                self.push_event(NetworkBehaviourAction::GenerateEvent(NetworkEvent::PeerLeft(peer)));
            }
        }
    }

    fn poll(&mut self, cx: &mut Context<'_>, _params: &mut impl PollParameters) -> Poll<NetworkBehaviourAction<HandlerInEvent, NetworkEvent<Peer>>> {
        // Emit custom events.
        if let Some(event) = self.events.pop_front() {
            //log::trace!("MessageBehaviour::poll: Emitting event: {:?}", event);
            return Poll::Ready(event);
        }
        
        // Remember the waker and then return Pending
        if self.waker.is_none() {
            self.waker = Some(cx.waker().clone());
        }

        Poll::Pending
    }
}
