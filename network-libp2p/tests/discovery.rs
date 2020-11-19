use std::{
    time::Duration,
    sync::Arc,
    collections::HashSet,
};

use libp2p::{
    identity::Keypair,
    core::{
        transport::MemoryTransport,
        multiaddr::{multiaddr, Multiaddr, Protocol},
        upgrade::Version,
        muxing::StreamMuxerBox,
    },
    secio::SecioConfig,
    swarm::{Swarm, NetworkBehaviour},
    yamux, PeerId, Transport
};
use futures::{
    future::Either,
    select, Stream, StreamExt
};
use parking_lot::RwLock;
use rand::{thread_rng, Rng, RngCore};

use nimiq_network_libp2p::discovery::{
    behaviour::{Discovery, DiscoveryConfig, DiscoveryEvent},
    peer_contacts::{PeerContact, Services, Protocols},
};
use nimiq_hash::Blake2bHash;
use nimiq_network_libp2p::discovery::peer_contacts::{PeerContactBook, SignedPeerContact};


struct TestNode {
    keypair: Keypair,
    peer_id: PeerId,
    swarm: Swarm<Discovery>,
    peer_contact_book: Arc<RwLock<PeerContactBook>>,
    address: Multiaddr,
}

impl TestNode {
    pub fn new() -> Self {
        let keypair = Keypair::generate_ed25519();
        let peer_id = PeerId::from(keypair.public());

        let base_transport = MemoryTransport::default();
        let address = multiaddr![Memory(thread_rng().gen::<u64>())];

        log::info!("Peer: id={}, address={}", peer_id, address);

        let transport = base_transport
            .upgrade(Version::V1)
            //.upgrade(Version::V1Lazy) // Allows for 0-RTT negotiation
            .authenticate(SecioConfig::new(keypair.clone()))
            .multiplex(yamux::Config::default())
            .map(|(peer, muxer), _| (peer, StreamMuxerBox::new(muxer)))
            .timeout(Duration::from_secs(20));

        let config = DiscoveryConfig {
            genesis_hash: Blake2bHash::default(),
            update_interval: Duration::from_secs(10),
            min_send_update_interval: Duration::from_secs(5),
            update_limit: Some(64),
            protocols_filter: Protocols::all(),
            services_filter: Services::all(),
            min_recv_update_interval: Duration::from_secs(1),
        };

        let peer_contact = PeerContact {
            addresses: Some(address.clone()).into_iter().collect(),
            public_key: keypair.public().clone(),
            services: Services::FULL_BLOCKS,
            timestamp: None,
        }.sign(&keypair);

        let peer_contact_book = Arc::new(RwLock::new(PeerContactBook::new(Default::default(), peer_contact)));

        let behaviour = Discovery::new(config, keypair.clone(), Arc::clone(&peer_contact_book));

        let mut swarm = Swarm::new(transport, behaviour, peer_id.clone());

        Swarm::listen_on(&mut swarm, address.clone()).unwrap();

        TestNode {
            keypair,
            peer_id,
            swarm,
            peer_contact_book,
            address,
        }
    }

    pub fn dial(&mut self, address: Multiaddr) {
        Swarm::dial_addr(&mut self.swarm, address).unwrap();
    }

    pub fn dial_peer_id(&mut self, peer_id: &PeerId) {
        Swarm::dial(&mut self.swarm, peer_id).unwrap();
    }
}


fn random_peer_contact(n: usize, services: Services) -> SignedPeerContact {
    let keypair = Keypair::generate_ed25519();

    let mut peer_contact = PeerContact {
        addresses: vec![format!("/dns/test{}.local/tcp/443/wss", n).parse().unwrap()].into_iter().collect(),
        public_key: keypair.public().clone(),
        services,
        timestamp: None,
    };

    peer_contact.set_current_time();

    peer_contact.sign(&keypair)
}

fn test_peers_in_contact_book(peer_contact_book: &PeerContactBook, peer_contacts: &[SignedPeerContact]) {
    for peer_contact in peer_contacts {
        let peer_id = peer_contact.public_key().clone().into_peer_id();
        log::info!("Checking if peer ID is in peer contact book: {}", peer_id);
        let peer_contact_in_book = peer_contact_book.get(&peer_id).expect("Peer ID not found");
        assert_eq!(peer_contact, peer_contact_in_book.signed(), "peer contacts differ");
    }
}


#[tokio::test]
pub async fn test_exchanging_peers() {
    //pretty_env_logger::init();

    // create nodes
    let mut node1 = TestNode::new();
    let mut node2 = TestNode::new();

    let peer_contact_book1 = Arc::clone(&node1.peer_contact_book);
    let peer_contact_book2 = Arc::clone(&node2.peer_contact_book);

    // known peer contacts of the first node
    let mut node1_peer_contacts = vec![
        random_peer_contact(10, Services::FULL_BLOCKS),
        random_peer_contact(11, Services::FULL_BLOCKS | Services::BLOCK_HISTORY),
        random_peer_contact(12, Services::BLOCK_PROOF),
    ];

    // known peer contacts of the first node
    let mut node2_peer_contacts = vec![
        random_peer_contact(13, Services::FULL_BLOCKS),
        random_peer_contact(14, Services::FULL_BLOCKS | Services::BLOCK_HISTORY),
        random_peer_contact(15, Services::CHAIN_PROOF | Services::ACCOUNTS_PROOF),
    ];

    // insert peers into node's contact books
    peer_contact_book1.write().insert_all(node1_peer_contacts.clone());
    peer_contact_book2.write().insert_all(node2_peer_contacts.clone());

    // connect
    node1.dial(node2.address.clone());

    // Run swarm for some time
    let mut t = 0;
    futures::stream::select(node1.swarm, node2.swarm)
        .take_while(move |e| {
            println!("Swarm event: {:?}", e);

            if let DiscoveryEvent::Update = e {
                t += 1;
            }

            async move { t < 2 }
        })
        .for_each(|_| async {})
        .await;

    let mut all_peer_contacts = vec![];
    all_peer_contacts.append(&mut node1_peer_contacts);
    all_peer_contacts.append(&mut node2_peer_contacts);

    log::info!("Checking peer 1 contact book.");
    test_peers_in_contact_book(&peer_contact_book1.read(), &all_peer_contacts);
    log::info!("Checking peer 2 contact book.");
    test_peers_in_contact_book(&peer_contact_book2.read(), &all_peer_contacts);
}

#[tokio::test]
pub async fn test_dialing_peer_from_contacts() {
    //pretty_env_logger::init();

    // create nodes
    let mut node1 = TestNode::new();
    let mut node2 = TestNode::new();

    let peer_contact_book1 = Arc::clone(&node1.peer_contact_book);
    let peer_contact_book2 = Arc::clone(&node2.peer_contact_book);

    let peer2_contact = peer_contact_book2.read().get_self().signed().clone();
    let peer2_id = node2.peer_id.clone();

    // insert peer address of node 2 into node 1's address book
    peer_contact_book1.write().insert(peer2_contact);

    // Dial node 2 from node 1 using only peer ID.
    node1.dial_peer_id(&peer2_id);

    // Just run node 2
    tokio::spawn(async move {
        node2.swarm.for_each(|_| async {}).await;
    });

    if let DiscoveryEvent::Established { peer_id } = node1.swarm.next().await {
        log::info!("Established PEX with {}", peer_id);
        assert_eq!(peer2_id, peer_id);
    }
}
