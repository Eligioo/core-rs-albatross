use futures::task::{Context, Poll};
use libp2p::{
    swarm::{KeepAlive, ProtocolsHandler, ProtocolsHandlerEvent, ProtocolsHandlerUpgrErr, SubstreamProtocol},
    PeerId,
};
use thiserror::Error;
use super::protocol::LimitProtocol;

#[derive(Clone, Debug)]
pub enum HandlerInEvent {
    ClosePeer { peer_id: PeerId },
}

#[derive(Clone, Debug)]
pub enum HandlerOutEvent {
    ClosePeers { peers: Vec<PeerId> },
}

#[derive(Debug, Error)]
pub enum HandlerError {}

#[derive(Default)]
pub struct LimitHandler {
    /// Peers we have to close.
    close_peers: Vec<PeerId>,
}

impl ProtocolsHandler for LimitHandler {
    type InEvent = HandlerInEvent;
    type OutEvent = HandlerOutEvent;
    type Error = HandlerError;
    type InboundProtocol = LimitProtocol;
    type OutboundProtocol = LimitProtocol;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<LimitProtocol, ()> {
        log::debug!("LimitHandler::listen_protocol");
        SubstreamProtocol::new(LimitProtocol, ())
    }

    fn inject_fully_negotiated_inbound(&mut self, _protocol: (), _info: ()) {
        log::debug!("LimitHandler::inject_fully_negotiated_inbound");
        todo!();
    }

    fn inject_fully_negotiated_outbound(&mut self, _protocol: (), _info: ()) {
        todo!();
    }

    fn inject_event(&mut self, event: HandlerInEvent) {
        log::debug!("MessageHandler::inject_event: {:?}", event);

        match event {
            HandlerInEvent::ClosePeer { peer_id } => {
                self.close_peers.push(peer_id);
            }
        }
    }

    fn inject_dial_upgrade_error(&mut self, _info: Self::OutboundOpenInfo, error: ProtocolsHandlerUpgrErr<std::io::Error>) {
        log::warn!("DiscoveryHandler::inject_dial_upgrade_error: {:?}", error);
        unimplemented!();
    }

    fn connection_keep_alive(&self) -> KeepAlive {
        KeepAlive::Yes
    }

    fn poll(&mut self, cx: &mut Context) -> Poll<ProtocolsHandlerEvent<Self::OutboundProtocol, (), HandlerOutEvent, HandlerError>> {
        if self.close_peers.len() > 0 {
            return Poll::Ready(ProtocolsHandlerEvent::Custom(HandlerOutEvent::ClosePeers { peers: self.close_peers.clone() } ));
        }

        // We do nothing in the handler
        Poll::Pending
    }
}
