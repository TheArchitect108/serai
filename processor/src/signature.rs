use std::collections::HashMap;
use std::{env, str, fmt};
use rdkafka::{
  producer::{BaseRecord, ThreadedProducer},
  consumer::{BaseConsumer, Consumer},
  ClientConfig, Message,
};
use message_box::MessageBox;
use std::time::Duration;
use log::info;

use serde::{Deserialize};
use crate::core::KafkaConfig;

#[derive(Debug, PartialEq, Eq, Hash)]
enum MessageType {
  CoordinatorPubkeyToProcessor,
  CoordinatorGeneralMessageToProcessor,
  CoordinatorSecureTestMessageToProcessor,
  ProcessorPubkeyToCoordinator,
  ProcessorGeneralMessageToCoordinator,
  ProcessorSecureTestMessageToCoordinator,
  Default,
}

impl fmt::Display for MessageType {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    match self {
      MessageType::CoordinatorPubkeyToProcessor => write!(f, "coordinator_pubkey_to_processor"),
      MessageType::CoordinatorGeneralMessageToProcessor => {
        write!(f, "coordinator_general_message_to_processor")
      }
      MessageType::CoordinatorSecureTestMessageToProcessor => {
        write!(f, "coordinator_secure_test_message_to_processor")
      }
      MessageType::ProcessorPubkeyToCoordinator => write!(f, "processor_pubkey_to_coordinator"),
      MessageType::ProcessorGeneralMessageToCoordinator => {
        write!(f, "processor_general_message_to_coordinator")
      }
      MessageType::ProcessorSecureTestMessageToCoordinator => {
        write!(f, "processor_secure_test_message_to_coordinator")
      }
      MessageType::Default => write!(f, "Default"),
    }
  }
}

fn parse_message_type(message_type: &str) -> MessageType {
  let mut msg_type = MessageType::Default;
  match message_type {
    "coordinator_pubkey_to_processor" => {
      msg_type = MessageType::CoordinatorPubkeyToProcessor;
    }
    "coordinator_general_message_to_processor" => {
      msg_type = MessageType::CoordinatorGeneralMessageToProcessor;
    }
    "coordinator_secure_test_message_to_processor" => {
      msg_type = MessageType::CoordinatorSecureTestMessageToProcessor;
    }
    "processor_pubkey_to_coordinator" => {
      msg_type = MessageType::ProcessorPubkeyToCoordinator;
    }
    "processor_general_message_to_coordinator" => {
      msg_type = MessageType::ProcessorGeneralMessageToCoordinator;
    }
    "processor_secure_test_message_to_coordinator" => {
      msg_type = MessageType::ProcessorSecureTestMessageToCoordinator;
    }
    _ => {}
  }
  msg_type
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
enum PartitionType {
  General,
  Secure,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SignatureProcess {
  coin: String,
  kafka_config: KafkaConfig,
  name: String,
}

// SignatureProcess communicates General & Secure Messages using Kafak
// General Messages will contain communicated pubkeys & general messages
// General Messages are contained in partition 0
// Secure Messages are contained in parition 1
impl SignatureProcess {
  pub fn new(coin: String, kafka_config: KafkaConfig, name: String) -> Self {
    info!("New Signature Process");
    Self { coin: coin, name: name, kafka_config: kafka_config }
  }

  pub async fn run(self) {
    info!("Starting Signature Process");

    // Initialize consumers to read the coordinator pubkey & general test messages on partition 0
    consume_general_messages_from_coordinator(&self.kafka_config, &self.name, &self.coin);

    // Initialize producer to send processor pubkeys to coordinator on general partition
    produce_processor_pubkey(&self.kafka_config, &self.name, &self.coin);

    // Wait to receive Coordinator Pubkey
    process_received_pubkey().await;

    // Initialize consumer used to read secure test messages from coordinator on secure partition
    consume_coordinator_secure_test_message(&self.kafka_config, &self.name, &self.coin);

    // Initialize a producer that sends a general & secure test message
    produce_general_and_secure_test_message(&self.kafka_config, &self.name, &self.coin).await;
  }

  fn stop(self) {
    info!("Stopping Signature Process");
  }
}

// Initialize consumers to read the coordinator pubkey & general test messages on partition 0
fn consume_general_messages_from_coordinator(kafka_config: &KafkaConfig, name: &str, coin: &str) {
  let mut group_id = &coin.to_string().to_lowercase();
  let mut topic: String = String::from(name);
  topic.push_str("_processor_");
  topic.push_str(&coin.to_string().to_lowercase());
  let env_key = &mut coin.to_string().to_owned().to_uppercase();
  env_key.push_str("_PUB");
  initialize_consumer(
    kafka_config,
    &group_id,
    &topic,
    Some(env_key.to_string()),
    None,
    &PartitionType::General,
  );
}

// Initialize consumer used to read secure test messages from coordinator on secure partition
fn consume_coordinator_secure_test_message(kafka_config: &KafkaConfig, name: &str, coin: &str) {
  let mut group_id = &coin.to_string().to_lowercase();
  let mut topic: String = String::from(name);
  topic.push_str("_processor_");
  topic.push_str(&coin.to_string().to_lowercase());
  let env_key = &mut coin.to_string().to_uppercase();
  env_key.push_str("_PRIV");
  initialize_consumer(
    kafka_config,
    &group_id,
    &topic,
    Some(env_key.to_string()),
    Some(&coin.to_string()),
    &PartitionType::Secure,
  );
}

// Initializes consumer based on general or secure partition
fn initialize_consumer(
  kafka_config: &KafkaConfig,
  group_id: &str,
  topic: &str,
  env_key: Option<String>,
  coin: Option<&String>,
  partition_type: &PartitionType,
) {
  let consumer: BaseConsumer = ClientConfig::new()
    .set("bootstrap.servers", format!("{}:{}", kafka_config.host, kafka_config.port))
    .set("group.id", group_id)
    .set("auto.offset.reset", kafka_config.offset_reset.to_owned())
    .create()
    .expect("invalid consumer config");

  let mut env_key_ref: String = "".to_string();
  match env_key {
    Some(p) => {
      env_key_ref = String::from(p);
    }
    None => {}
  }

  let mut coin_ref: String = "".to_string();
  match coin {
    Some(p) => {
      coin_ref = String::from(p);
    }
    None => {}
  }

  match partition_type {
    PartitionType::General => {
      let mut tpl = rdkafka::topic_partition_list::TopicPartitionList::new();
      tpl.add_partition(&topic, 0);
      consumer.assign(&tpl).unwrap();

      tokio::spawn(async move {
        for msg_result in &consumer {
          let msg = msg_result.unwrap();
          let key: &str = msg.key_view().unwrap().unwrap();
          let msg_type = parse_message_type(&key);
          match msg_type {
            MessageType::CoordinatorPubkeyToProcessor => {
              let coord_pubkey_check = env::var("COORD_PUB");
              if coord_pubkey_check.is_err() {
                let value = msg.payload().unwrap();
                let public_key = str::from_utf8(value).unwrap();
                info!("Received {} Public Key: {}", &key, &public_key);
                env::set_var("COORD_PUB", public_key);
              }
            }
            MessageType::CoordinatorGeneralMessageToProcessor => {
              let value = msg.payload().unwrap();
              let pub_msg = str::from_utf8(value).unwrap();
              info!("Received Public Message from {}", &key);
              info!("Public Message: {}", &pub_msg);
            }
            _ => {}
          }
        }
      });
    }
    PartitionType::Secure => {
      let mut tpl = rdkafka::topic_partition_list::TopicPartitionList::new();
      tpl.add_partition(&topic, 1);
      consumer.assign(&tpl).unwrap();

      tokio::spawn(async move {
        for msg_result in &consumer {
          let msg = msg_result.unwrap();
          let key: &str = msg.key_view().unwrap().unwrap();
          let msg_type = parse_message_type(&key);
          match msg_type {
            MessageType::CoordinatorSecureTestMessageToProcessor => {
              let value = msg.payload().unwrap();
              // Creates Message box used for decryption
              let pubkey = message_box::PublicKey::from_trusted_str(
                &env::var("COORD_PUB").unwrap().to_string(),
              );

              let coin_priv = message_box::PrivateKey::from_string(
                env::var(env_key_ref.to_string()).unwrap().to_string(),
              );

              let processor_id = retrieve_message_box_id(&coin_ref.to_uppercase());

              let mut message_box_pubkeys = HashMap::new();
              message_box_pubkeys.insert(message_box::ids::COORDINATOR, pubkey);

              let message_box = MessageBox::new(processor_id, coin_priv, message_box_pubkeys);
              let encrypted_msg = str::from_utf8(value).unwrap();

              // Decrypt message using Message Box
              let encoded_string = message_box
                .decrypt_from_str(&message_box::ids::COORDINATOR, &encrypted_msg)
                .unwrap();
              let decoded_string = String::from_utf8(encoded_string).unwrap();
              info!("Received Encrypted Message from {}", &key);
              info!("Decrypted Message: {}", &decoded_string);
            }
            _ => {}
          }
        }
      });
    }
    _ => {}
  }
}

// Initialize producer to send processor pubkeys to coordinator on general partition
fn produce_processor_pubkey(kafka_config: &KafkaConfig, name: &str, coin: &str) {
  let mut topic: String = String::from(name);
  topic.push_str("_processor_");
  topic.push_str(&coin.to_string().to_lowercase());
  let env_key = &mut coin.to_string().to_uppercase();
  env_key.push_str("_PUB");
  send_processor_pubkey(&kafka_config, &topic, env_key.to_string(), coin);
}

// Sends processor pubkeys to coordinator on general partition
fn send_processor_pubkey(kafka_config: &KafkaConfig, topic: &str, env_key: String, coin: &str) {
  let producer: ThreadedProducer<_> = ClientConfig::new()
    .set("bootstrap.servers", format!("{}:{}", kafka_config.host, kafka_config.port))
    .create()
    .expect("invalid producer config");

  // Load Processor Pubkeys
  let coin_pub = env::var(env_key.to_string());
  let coin_msg = coin_pub.unwrap();

  info!("Sending {} Public Key to Corodinator", coin.to_uppercase());

  // Send pubkey to kafka topic
  producer
    .send(
      BaseRecord::to(&topic)
        .key(&format!("{}", MessageType::ProcessorPubkeyToCoordinator.to_string()))
        .payload(&coin_msg)
        .partition(0),
    )
    .expect("failed to send message");
}

// Wait to receive Coordinator Pubkey
async fn process_received_pubkey() {
  // Runs a loop to check if Coordinator pubkey is found
  let mut coord_key_found = false;
  while !coord_key_found {
    let coord_pub_check = env::var("COORD_PUB");
    if !coord_pub_check.is_err() {
      info!("Coord Pubkey Ready");
      coord_key_found = true;
    } else {
      // Add small delay for checking pubkeys
      tokio::time::sleep(Duration::from_millis(500)).await;
    }
  }
}

// Requests Coin ID from Message Box
fn retrieve_message_box_id(coin: &String) -> &'static str {
  let id = match coin.as_str() {
    "SRI" => message_box::ids::SRI_PROCESSOR,
    "BTC" => message_box::ids::BTC_PROCESSOR,
    "ETH" => message_box::ids::ETH_PROCESSOR,
    "XMR" => message_box::ids::XMR_PROCESSOR,
    &_ => "",
  };
  id
}

// Initialize a producer that sends a general & secure test message
async fn produce_general_and_secure_test_message(
  kafka_config: &KafkaConfig,
  name: &str,
  coin: &str,
) {
  let mut topic: String = String::from(name);
  topic.push_str("_processor_");
  topic.push_str(&coin.to_string().to_lowercase());
  let env_key = &mut coin.to_string().to_uppercase();
  env_key.push_str("_PRIV");

  let processor_id = retrieve_message_box_id(&coin.to_string().to_uppercase());
  let mut msg: String = "".to_string();
  msg.push_str(&processor_id.to_lowercase());
  msg.push_str(
    &format!(" message to {}", String::from(message_box::ids::COORDINATOR)).to_lowercase(),
  );

  send_general_and_secure_test_message(
    &kafka_config,
    &topic,
    env_key.to_string(),
    &processor_id,
    msg.as_bytes().to_vec(),
  )
  .await;
}

// Initializes a producer then sends both a general and secure test message
async fn send_general_and_secure_test_message(
  kafka_config: &KafkaConfig,
  topic: &str,
  env_key: String,
  processor: &'static str,
  msg: Vec<u8>,
) {
  let producer: ThreadedProducer<_> = ClientConfig::new()
    .set("bootstrap.servers", format!("{}:{}", kafka_config.host, kafka_config.port))
    .create()
    .expect("invalid producer config");

  // Load Processor private key environment variable
  let coin_priv =
    message_box::PrivateKey::from_string(env::var(env_key.to_string()).unwrap().to_string());

  // Load Pubkeys for processors
  let pubkey =
    message_box::PublicKey::from_trusted_str(&env::var("COORD_PUB").unwrap().to_string());
  let mut message_box_pubkey = HashMap::new();
  message_box_pubkey.insert(message_box::ids::COORDINATOR, pubkey);

  // Create Coordinator Message Box
  let message_box = MessageBox::new(processor, coin_priv, message_box_pubkey);
  let enc = message_box.encrypt_to_string(&message_box::ids::COORDINATOR, &msg.clone());

  // Parition 0 is General
  producer
    .send(
      BaseRecord::to(&topic)
        .key(&format!("{}", MessageType::ProcessorGeneralMessageToCoordinator.to_string()))
        .payload(&msg)
        .partition(0),
    )
    .expect("failed to send message");

  // Partition 1 is Secure
  producer
    .send(
      BaseRecord::to(&topic)
        .key(&format!("{}", MessageType::ProcessorSecureTestMessageToCoordinator.to_string()))
        .payload(&enc)
        .partition(1),
    )
    .expect("failed to send message");
  // Add small delay for checking pubkeys
  tokio::time::sleep(Duration::from_millis(500)).await;
}
