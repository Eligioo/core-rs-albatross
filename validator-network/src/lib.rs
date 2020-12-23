#[macro_use]
extern crate beserial_derive;

pub mod error;
pub mod network_impl;

use std::{pin::Pin, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::Stream;

use nimiq_network_interface::{
    network::Topic,
    message::Message,
    peer::Peer,
};
use nimiq_bls::{CompressedPublicKey, SecretKey};

pub use crate::error::NetworkError;

pub type MessageStream<TMessage, TPeerId> = Pin<Box<dyn Stream<Item = (TMessage, TPeerId)> + Send + 'static>>;

/// Fixed upper bound network.
/// Peers are denoted by a usize identifier which deterministically identifies them.
#[async_trait]
pub trait ValidatorNetwork: Send + Sync {
    type Error: std::error::Error;
    type PeerType: Peer;

    async fn get_validator_peer(&self, validator_id: usize) -> Result<Option<Arc<Self::PeerType>>, Self::Error>;

    /// must make a reasonable efford to establish a connection to the peer denoted with `validator_id`
    /// before returning a connection not established error.
    async fn send_to<M: Message>(&self, validator_ids: &[usize], msg: &M) -> Vec<Result<(), Self::Error>>;

    /// Will receive from all connected peers
    fn receive<M: Message>(&self) -> MessageStream<M, <Self::PeerType as Peer>::Id>;

    async fn publish<TTopic: Topic + Sync>(&self, topic: &TTopic, item: TTopic::Item) -> Result<(), Self::Error>;

    async fn subscribe<TTopic: Topic + Sync>(
        &self,
        topic: &TTopic,
    ) -> Result<Pin<Box<dyn Stream<Item = (TTopic::Item, <Self::PeerType as Peer>::Id)> + Send>>, Self::Error>;

    /// registers a cache for the specified message type.
    /// Incoming messages of this type shuld be held in a FIFO queue of total size `buffer_size`, each with a lifetime of `lifetime`
    /// `lifetime` or `buffer_size` of 0 should disable the cache.
    fn cache<M: Message>(&self, buffer_size: usize, lifetime: Duration);

    async fn set_public_key(&self, public_key: &CompressedPublicKey, secret_key: &SecretKey) -> Result<(), Self::Error>;
}
