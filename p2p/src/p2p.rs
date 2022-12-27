use std::{collections::HashMap, process, env, time::Duration, num::NonZeroU32};
use tokio::{sync::mpsc, signal::ctrl_c};
use serde::{Deserialize};
use log::{error, info};
use config::{Config, ConfigError, File};
use futures::StreamExt;
use libp2p::{
  core::{upgrade},
  floodsub::{Floodsub, FloodsubEvent, Topic},
  identity::{self},
  mplex,
  noise::{Keypair, NoiseConfig, X25519Spec},
  swarm::NetworkBehaviourEventProcess,
  tcp::TcpConfig,
  NetworkBehaviour, PeerId, Swarm, Transport, Multiaddr, mdns::{MdnsEvent, MdnsConfig, Mdns},
};
use crate::{state::{P2P_Message, MessageType, State}, history::History};


#[derive(NetworkBehaviour)]
#[behaviour(event_process = true)]
struct P2PConnection {
  dns: Mdns,
  floodsub: Floodsub,
  #[behaviour(ignore)]
  state: State,
  #[behaviour(ignore)]
  peer_id: String,
  #[behaviour(ignore)]
  responder: mpsc::UnboundedSender<P2P_Message>,
}

impl NetworkBehaviourEventProcess<MdnsEvent> for P2PConnection {
  fn inject_event(&mut self, event: MdnsEvent) {
    match event {
      MdnsEvent::Discovered(nodes) => {
        //New node found!
        for (peer, addr) in nodes {
          //XXX Do we need a condition check to see if this node already exists?
          info!("Peer {} found at {}", peer, addr);
          self.floodsub.add_node_to_partial_view(peer);
        }
      }
      MdnsEvent::Expired(nodes) => {
        //Do we *need* to handle this? What's the risk?
        //They'll just build up right...
        for (peer, addr) in nodes {
          if !self.dns.has_node(&peer) {
            //Why this check?
            info!("Peer {} disconnected at {}", peer, addr);
            self.floodsub.remove_node_from_partial_view(&peer);
          }
        }
      }
    }
  }
}

impl NetworkBehaviourEventProcess<FloodsubEvent> for P2PConnection {
  fn inject_event(&mut self, event: FloodsubEvent) {
    match event {
      FloodsubEvent::Message(raw_data) => {
        //Parse the message as bytes
        let deser = bincode::deserialize::<P2P_Message>(&raw_data.data);
        //dbg!(&deser);
        if let Ok(message) = deser {
          if let Some(user) = &message.addressee {
            if *user != self.peer_id.to_string() {
              return; //Don't process messages not intended for us.
            }
          }

          match message.message_type {
            MessageType::Message => {
              println!("Message recieved!");
              let username: String = self.state.get_username(&raw_data.source.to_string());
              println!("{}: {}", username, String::from_utf8_lossy(&message.data));

              //Store message in history
              self.state.history.insert(message);
            }
            MessageType::State => {
              println!("History recieved!");
              let data: State = bincode::deserialize(&message.data).unwrap();
              //dbg!(&data);
              self.state.merge(data);
            }
          }
        } else {
          error!("Unable to decode message! Due to {:?}", deser.unwrap_err());
        }
      }
      FloodsubEvent::Subscribed { peer_id, topic: _ } => {
        //Send our state to new user
        println!("Sending stage to {}", peer_id);
        let message: P2P_Message = P2P_Message {
          message_type: MessageType::State,
          data: bincode::serialize(&self.state).unwrap(),
          addressee: Some(peer_id.to_string()),
          source: self.peer_id.to_string(),
        };
        send_response(message, self.responder.clone());
      }
      FloodsubEvent::Unsubscribed { peer_id, topic: _ } => {
        let name =
          self.state.usernames.remove(&peer_id.to_string()).unwrap_or(String::from("Anon"));
        println!("Disconnect from {}", name);
      }
    }
  }
}

#[derive(Clone, Debug, Deserialize)]
pub struct P2PProcess {
  name: String,
  address: String,
  address_book: String,
}

impl P2PProcess {
  pub fn new(name: String, path: String) -> Self {
    info!("New P2P Process");

    let config = Config::builder()
      .add_source(File::with_name(&format!("{}/development", path)))
      .build()
      .unwrap();

    let address = config.get_string(&format!("{}.address", name)).unwrap();
    let address_book = config.get_string(&format!("{}.address_book", name)).unwrap();

    Self { address, address_book, name }
  }

  pub async fn run(self) {
    info!("Starting P2P Process");

    let id_keys = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(id_keys.public());
  
    println!("Peer id: {}", &peer_id);

    let auth_keys = Keypair::<X25519Spec>::new()
      .into_authentic(&id_keys)
      .expect("unable to create authenticated keys");

    let transport = TcpConfig::new()
      .upgrade(upgrade::Version::V1)
      .authenticate(NoiseConfig::xx(auth_keys).into_authenticated())
      .multiplex(mplex::MplexConfig::new())
      .boxed();

    //Generate channel
    let (response_sender, mut response_rcv) = mpsc::unbounded_channel();

    let mut behaviour = P2PConnection {
      dns: Mdns::new(MdnsConfig::default()).await.expect("unable to create mdns"),
      floodsub: Floodsub::new(peer_id),
      state: State {
        history: History::new(),
        usernames: HashMap::from([(peer_id.to_string(), peer_id.to_string())]),
      },
      peer_id: peer_id.to_string(),
      responder: response_sender,
    };

    let topic = Topic::new("coordinator");
    behaviour.floodsub.subscribe(topic.clone());

    let mut swarm = Swarm::new(transport, behaviour, peer_id);
    swarm.listen_on(self.address.parse().unwrap());
    swarm.dial(self.address_book.parse::<Multiaddr>().unwrap());
    println!("Dialed {}", &self.address_book);

    let mut message_ready = true;
    let mut connection_established = false;

    loop {
      tokio::select! {
        is_ready = delay_until_ready(message_ready) => {
          if is_ready && connection_established {
            let msg = format!("Message from {}", &self.name);
            let message: P2P_Message = P2P_Message {
              message_type: MessageType::Message,
              data: msg.as_bytes().to_vec(),
              addressee: None,
              source: peer_id.to_string(),
            };

            println!("Sending message");
            send_message(&message, &mut swarm, &topic);
            message_ready = false;
          }
        },
        event = swarm.select_next_some() => {
                println!("Swarm event: {:?}", &event);
                match event {
                  libp2p::swarm::SwarmEvent::Behaviour(_) => {
                    println!("Behavior Event");
                  }
                  libp2p::swarm::SwarmEvent::ConnectionEstablished { peer_id, endpoint, num_established, concurrent_dial_errors } => {
                    println!("Connection Established");
                    if num_established == NonZeroU32::new(1).unwrap() {
                      connection_established = true;
                    }
                  },
                  libp2p::swarm::SwarmEvent::ConnectionClosed { peer_id, endpoint, num_established, cause } => {
                    println!("Connection Closed");
                  }
                  libp2p::swarm::SwarmEvent::IncomingConnection { local_addr, send_back_addr } => {
                    println!("Incoming Connection");
                  }
                  libp2p::swarm::SwarmEvent::IncomingConnectionError { local_addr, send_back_addr, error } => {
                    println!("Incoming Error");
                  }
                  libp2p::swarm::SwarmEvent::OutgoingConnectionError { peer_id, error } => {
                    println!("Outgoing Connection Error");
                  }
                  libp2p::swarm::SwarmEvent::BannedPeer { peer_id, endpoint } => {
                    println!("Banned Peer");
                  }
                  libp2p::swarm::SwarmEvent::NewListenAddr { listener_id, address } => {
                    println!("New Listen Addr");
                  }
                  libp2p::swarm::SwarmEvent::ExpiredListenAddr { listener_id, address } => {
                    println!("Expired Listen Addr");
                  }
                  libp2p::swarm::SwarmEvent::ListenerClosed { listener_id, addresses, reason } => {
                    println!("Listener Closed");
                  }
                  libp2p::swarm::SwarmEvent::ListenerError { listener_id, error } => {
                    println!("Listener Error");
                  },
                  libp2p::swarm::SwarmEvent::Dialing(_) => {
                    println!("Dialing");
                  }
                }
        },
        response = response_rcv.recv() => {
            if let Some(message) = response {
                send_message(&message, &mut swarm, &topic);
            }
        },
        event = ctrl_c() => {
            if let Err(e) = event {
                println!("Failed to register interrupt handler {}", e);
            }
            break;
        }
      }
    }

    process::exit(0);
  }

  fn stop(self) {
    info!("Stopping P2P Process");
  }
}

async fn delay_until_ready(message_sent: bool) -> bool {
    tokio::time::sleep(Duration::from_millis(1000)).await;
    message_sent
  }

  fn send_response(message: P2P_Message, sender: mpsc::UnboundedSender<P2P_Message>) {
    tokio::spawn(async move {
      if let Err(e) = sender.send(message) {
        error!("error sending response via channel {}", e);
      }
    });
  }
  
  /// Send a message using the swarm
  fn send_message(message: &P2P_Message, swarm: &mut Swarm<P2PConnection>, topic: &Topic) {
    let bytes = bincode::serialize(message).unwrap();
    swarm.behaviour_mut().floodsub.publish(topic.clone(), bytes);
  }
