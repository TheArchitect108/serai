mod history;
mod state;

use futures::StreamExt;
use group::ff::PrimeField;
use history::History;
use libp2p::{
  core::upgrade,
  floodsub::{Floodsub, FloodsubEvent, Topic},
  identity::{self},
  mdns::{Mdns, MdnsConfig, MdnsEvent},
  mplex,
  noise::{Keypair, NoiseConfig, X25519Spec},
  swarm::NetworkBehaviourEventProcess,
  tcp::TcpConfig,
  NetworkBehaviour, PeerId, Swarm, Transport,
};
use log::{error, info};
use message_box::MessageBox;
use state::{P2P_Message, MessageType, State};
use std::{collections::HashMap, process, time::Duration, num::NonZeroU32};
use tokio::{sync::mpsc, signal::ctrl_c};

use rdkafka::{
  consumer::{BaseConsumer, Consumer},
  producer::{BaseRecord, ProducerContext, ThreadedProducer},
  ClientConfig, ClientContext, Offset, Message,
};

use std::env;

// Pass in service_id with Args
// cargo run -- service_id Coordinator
// cargo run -- service_id BTC

/// Transmit a response to other peers utilizing the channels
fn send_response(message: P2P_Message, sender: mpsc::UnboundedSender<P2P_Message>) {
  tokio::spawn(async move {
    if let Err(e) = sender.send(message) {
      error!("error sending response via channel {}", e);
    }
  });
}

/// Send a message using the swarm
fn send_message(message: &P2P_Message, swarm: &mut Swarm<Chat>, topic: &Topic) {
  let bytes = bincode::serialize(message).unwrap();
  swarm.behaviour_mut().messager.publish(topic.clone(), bytes);
}

#[derive(NetworkBehaviour)]
#[behaviour(event_process = true)]
struct Chat {
  dns: Mdns,
  messager: Floodsub,
  #[behaviour(ignore)]
  state: State,
  #[behaviour(ignore)]
  peer_id: String,
  #[behaviour(ignore)]
  responder: mpsc::UnboundedSender<P2P_Message>,
}

impl NetworkBehaviourEventProcess<MdnsEvent> for Chat {
  fn inject_event(&mut self, event: MdnsEvent) {
    match event {
      MdnsEvent::Discovered(nodes) => {
        //New node found!
        for (peer, addr) in nodes {
          //XXX Do we need a condition check to see if this node already exists?
          info!("Peer {} found at {}", peer, addr);
          self.messager.add_node_to_partial_view(peer);
        }
      }
      MdnsEvent::Expired(nodes) => {
        //Do we *need* to handle this? What's the risk?
        //They'll just build up right...
        for (peer, addr) in nodes {
          if !self.dns.has_node(&peer) {
            //Why this check?
            info!("Peer {} disconnected at {}", peer, addr);
            self.messager.remove_node_from_partial_view(&peer);
          }
        }
      }
    }
  }
}

impl NetworkBehaviourEventProcess<FloodsubEvent> for Chat {
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
              info!("Message recieved!");
              let username: String = self.state.get_username(&raw_data.source.to_string());
              println!("{}: {}", username, String::from_utf8_lossy(&message.data));

              //Store message in history
              self.state.history.insert(message);
            }
            MessageType::State => {
              info!("History recieved!");
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
        info!("Sending stage to {}", peer_id);
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  pretty_env_logger::init();

  let mut args: Vec<String> = env::args().collect();
  let service_id = args.pop().unwrap();

  // Kafka Integration
  initialize_keys(service_id.as_str());
  create_pubkey_consumer(service_id.as_str());
  create_pubkey_producer(service_id.as_str());
  let mut pubkey_check = false;
  while !pubkey_check {
    pubkey_check = check_pubkeys(service_id.as_str()).await;
  }

  let id_keys = identity::Keypair::generate_ed25519();
  let peer_id = PeerId::from(id_keys.public());

  println!("{}: {}", &service_id, &peer_id);

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

  let mut behaviour = Chat {
    dns: Mdns::new(MdnsConfig::default()).await.expect("unable to create mdns"),
    messager: Floodsub::new(peer_id),
    state: State {
      history: History::new(),
      usernames: HashMap::from([(peer_id.to_string(), service_id.to_string())]),
    },
    peer_id: peer_id.to_string(),
    responder: response_sender,
  };

  let topic = Topic::new("sylo");
  behaviour.messager.subscribe(topic.clone());

  let mut swarm = Swarm::new(transport, behaviour, peer_id);
  swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

  let mut message_ready = true;
  let mut connection_established = false;

  loop {
    tokio::select! {
      is_ready = delay_until_ready(message_ready) => {
        if is_ready && connection_established {
          let msg = format!("Message from {}", &service_id);
          let enc_msg = build_encrypt_msg(&service_id, &msg.to_string());
          let message: P2P_Message = P2P_Message {
            message_type: MessageType::Message,
            data: enc_msg.as_bytes().to_vec(),
            addressee: None,
            source: peer_id.to_string(),
          };

          send_message(&message, &mut swarm, &topic);

          //Log Message
          swarm
              .behaviour_mut()
              .state
              .history
              .insert(message);

          message_ready = false;
        }
      },
      event = swarm.select_next_some() => {
              //println!("Swarm event: {:?}", &event);

              match event {
                libp2p::swarm::SwarmEvent::Behaviour(_) => {
                  println!("Behavior Event");
                }
                libp2p::swarm::SwarmEvent::ConnectionEstablished { peer_id, endpoint, num_established, concurrent_dial_errors } => {
                  println!("Connection Established");
                  if num_established == NonZeroU32::new(2).unwrap() {
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

  swarm.behaviour_mut().messager.unsubscribe(topic);

  //HACK workaround to force the unsubscribe to actually send.
  //Annoying as it causes a delay when closing the application.
  //swarm.select_next_some().await;

  process::exit(0);
}

async fn delay_until_ready(message_sent: bool) -> bool {
  tokio::time::sleep(Duration::from_millis(1000)).await;
  message_sent
}

// Generates Private / Public key pair
fn initialize_keys(service: &str) {
  // Checks if coordinator keys are set
  let coord_priv_check = env::var("COORDINATOR_PRIV");
  if coord_priv_check.is_err() {
    println!("Generating New Keys");
    // Generates new private / public key
    let (private, public) = message_box::key_gen();
    let private_bytes = unsafe { private.inner().to_repr() };

    let mut env_priv_key = service.to_string();
    env_priv_key = env_priv_key.to_uppercase();
    env_priv_key.push_str("_PRIV");

    dbg!(&env_priv_key);

    let mut env_pub_key = service.to_string();
    env_pub_key = env_pub_key.to_uppercase();
    env_pub_key.push_str("_PUB");

    // Sets private / public key to environment variables
    env::set_var(env_priv_key, hex::encode(&private_bytes.as_ref()));
    env::set_var(env_pub_key, hex::encode(&public.to_bytes()));
  }
}

fn create_pubkey_consumer(service: &str) {
  let consumer: BaseConsumer = ClientConfig::new()
    .set("bootstrap.servers", "localhost:9094")
    .set("group.id", "p2p")
    .set("auto.offset.reset", "smallest")
    .create()
    .expect("invalid consumer config");

  let service_ref = service.to_owned();

  let mut tpl = rdkafka::topic_partition_list::TopicPartitionList::new();
  tpl.add_partition(&"PUBKEY", 0);
  consumer.assign(&tpl).unwrap();

  tokio::spawn(async move {
    for msg_result in &consumer {
      let msg = msg_result.unwrap();
      let key: &str = msg.key_view().unwrap().unwrap();
      if !key.contains(&service_ref) {
        let value = msg.payload().unwrap();
        let public_key = std::str::from_utf8(value).unwrap();
        println!("Received Pubkey from {}: {}", &key, &public_key);
        let env_pubkey = &key.to_string();
        let env_pubkey_ref = &mut env_pubkey.to_uppercase();
        env_pubkey_ref.push_str("_PUB");
        println!("{}", &env_pubkey_ref);
        env::set_var(env_pubkey_ref.clone(), public_key);
      }
    }
  });
}

fn create_pubkey_producer(service: &str) {
  let producer: ThreadedProducer<_> = ClientConfig::new()
    .set("bootstrap.servers", "localhost:9094")
    .create()
    .expect("invalid producer config");

  let env_pub_key = &mut service.to_string().to_uppercase();
  env_pub_key.push_str("_PUB");

  // Load Coordinator Pubkey
  let pubkey = env::var(env_pub_key);
  let msg = pubkey.unwrap();

  // Sends message to Kafka
  producer
    .send(BaseRecord::to(&"PUBKEY").key(&format!("{}", &service)).payload(&msg).partition(0))
    .expect("failed to send message");
}

async fn check_pubkeys(service: &str) -> bool {
  let mut ret = false;

  if service.contains("Coordinator") {
    let pub_check = env::var("BTC_PUB");
    if !pub_check.is_err() {
      ret = true;
    }
  } else {
    let pub_check = env::var("COORDINATOR_PUB");
    if !pub_check.is_err() {
      ret = true;
    }
  }

  tokio::time::sleep(Duration::from_millis(1000)).await;
  ret
}

fn build_encrypt_msg(service: &str, msg: &str) -> String {
  dbg!(&service);
  if service.contains("COORDINATOR") {
    let btc_pubkey =
      message_box::PublicKey::from_trusted_str(&env::var("BTC_PUB").unwrap().to_string());

    let coord_priv =
      message_box::PrivateKey::from_string(env::var("COORDINATOR_PRIV").unwrap().to_string());

    let mut message_box_pubkey = HashMap::new();
    message_box_pubkey.insert("BTC", btc_pubkey);

    // Create Procesor Message Box
    let message_box = MessageBox::new("COORDINATOR", coord_priv, message_box_pubkey);
    return message_box.encrypt_to_string(&"BTC", &msg.clone());

  } else {
    let coord_pubkey =
      message_box::PublicKey::from_trusted_str(&env::var("COORDINATOR_PUB").unwrap().to_string());

    let btc_priv =
      message_box::PrivateKey::from_string(env::var("BTC_PRIV").unwrap().to_string());

    let mut message_box_pubkey = HashMap::new();
    message_box_pubkey.insert("COORDINATOR", coord_pubkey);
  
    // Create Procesor Message Box
    let message_box = MessageBox::new("BTC", btc_priv, message_box_pubkey);
    return message_box.encrypt_to_string(&"COORDINATOR", &msg.clone());
  }
}
