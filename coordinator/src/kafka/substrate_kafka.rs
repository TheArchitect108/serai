use std::{thread, time::Duration, collections::HashMap};
use std::{env, str};
use rdkafka::{
  consumer::{BaseConsumer, Consumer, ConsumerContext, Rebalance},
  message::ToBytes,
  producer::{BaseProducer, BaseRecord, Producer, ProducerContext, ThreadedProducer},
  ClientConfig, ClientContext, Message, Offset,
};
use message_box::{MessageBox, SecureMessage};

pub fn start() {
  println!("Starting Substrate Kafka Test");
  let consumer: BaseConsumer<ConsumerCallbackLogger> = ClientConfig::new()
    .set("bootstrap.servers", "localhost:9094")
    .set("group.id", "serai")
    .create_with_context(ConsumerCallbackLogger {})
    .expect("Substrate invalid consumer config");

  consumer.subscribe(&["coord_to_substrate_topic"]).expect("Substrate topic subscribe failed");

  thread::spawn(move || {
    for msg_result in &consumer {
      // Pulls recent messages from Kafka
      let msg = msg_result.unwrap();
      let key: &str = msg.key_view().unwrap().unwrap();
      let value = msg.payload().unwrap();

      // Converts message from kafka UI into encrytped string
      let encrypted_msg = str::from_utf8(value).unwrap();
      dbg!(&encrypted_msg);

      // Creates Message box used for decryption
      // I use REF to illustrate pulling env variables, there's existing A_PUB in scope
      let A_PUB =
        message_box::PublicKey::from_trusted_str(&env::var("SUBSTRATE_BOX_PUB").unwrap().to_string());

      let B_PRIV =
        message_box::PrivateKey::from_string(env::var("COORD_BOX_PRIV").unwrap().to_string());

      let mut b_others = HashMap::new();
      b_others.insert("Substrate_Processor", A_PUB);

      let b_box = MessageBox::new("Coordinator", B_PRIV, b_others);

      // Decrypt message using Message Box
      let encoded_string = b_box.decrypt_from_str(&"Substrate_Processor", &encrypted_msg).unwrap();
      let decoded_string = String::from_utf8(encoded_string).unwrap();
      dbg!(&decoded_string);
    }
  });

  kafka_send_msg();
}

pub fn kafka_send_msg() {
  // Parses ENV variables to proper priv/pub keys
  let A_PRIV =
    message_box::PrivateKey::from_string(env::var("SUBSTRATE_BOX_PRIV").unwrap().to_string());
  let A_PUB = message_box::PublicKey::from_trusted_str(&env::var("SUBSTRATE_BOX_PUB").unwrap().to_string());

  let B_PRIV =
    message_box::PrivateKey::from_string(env::var("COORD_BOX_PRIV").unwrap().to_string());
  let B_PUB = message_box::PublicKey::from_trusted_str(&env::var("COORD_BOX_PUB").unwrap().to_string());

  // Create a HashMap of each pair using service name and public key
  let mut a_others = HashMap::new();
  a_others.insert("Coordinator", B_PUB);

  let mut b_others = HashMap::new();
  b_others.insert("Substrate_Processor", A_PUB);

  // Initialize a MessageBox for each service
  let a_box = MessageBox::new("Substrate_Processor", A_PRIV, a_others);
  let b_box = MessageBox::new("Coordinator", B_PRIV, b_others);
  // Creates a producer to send message
  let producer: ThreadedProducer<ProduceCallbackLogger> = ClientConfig::new()
    .set("bootstrap.servers", "localhost:9094")
    .create_with_context(ProduceCallbackLogger {})
    .expect("Substrate invalid producer config");

  println!("Sending Submstrate message to Coordinator");

  // Creates a message & encryptes using Message Box
  let msg = b"Private Substrate Message to Coordinator".to_vec();
  let enc = a_box.encrypt_to_string(&"Coordinator", &msg.clone());

  // Sends message to Kafka
  producer
    .send(BaseRecord::to("substrate_to_coord_topic").key(&format!("node-user-{}", 1)).payload(&enc))
    .expect("Substrate failed to send message");

  thread::sleep(Duration::from_secs(3));
}

struct ConsumerCallbackLogger;

impl ClientContext for ConsumerCallbackLogger {}

impl ConsumerContext for ConsumerCallbackLogger {
  fn pre_rebalance<'a>(&self, _rebalance: &rdkafka::consumer::Rebalance<'a>) {}

  fn post_rebalance<'a>(&self, rebalance: &rdkafka::consumer::Rebalance<'a>) {
    println!("Substrate post_rebalance callback");

    match rebalance {
      Rebalance::Assign(tpl) => {
        for e in tpl.elements() {
          println!("Substrate rebalanced partition {}", e.partition())
        }
      }
      Rebalance::Revoke(tpl) => {
        println!("Substrate ALL partitions have been REVOKED")
      }
      Rebalance::Error(err_info) => {
        println!("Substrate Post Rebalance error {}", err_info)
      }
    }
  }

  fn commit_callback(
    &self,
    result: rdkafka::error::KafkaResult<()>,
    offsets: &rdkafka::TopicPartitionList,
  ) {
    match result {
      Ok(_) => {
        for e in offsets.elements() {
          match e.offset() {
            //skip Invalid offset
            Offset::Invalid => {}
            _ => {
              println!("Substrate committed offset {:?} in partition {}", e.offset(), e.partition())
            }
          }
        }
      }
      Err(err) => {
        println!("Substrate error committing offset - {}", err)
      }
    }
  }
}

struct ProduceCallbackLogger;

impl ClientContext for ProduceCallbackLogger {}

impl ProducerContext for ProduceCallbackLogger {
  type DeliveryOpaque = ();

  fn delivery(
    &self,
    delivery_result: &rdkafka::producer::DeliveryResult<'_>,
    _delivery_opaque: Self::DeliveryOpaque,
  ) {
    let dr = delivery_result.as_ref();
    let msg = dr.unwrap();

    match dr {
      Ok(msg) => {
        let key: &str = msg.key_view().unwrap().unwrap();
        println!(
          "Substrate Produced message with key {} in offset {} of partition {}",
          key,
          msg.offset(),
          msg.partition()
        );
      }
      Err(producer_err) => {
        let key: &str = producer_err.1.key_view().unwrap().unwrap();

        println!("Substrate failed to produce message with key {} - {}", key, producer_err.0,)
      }
    }
  }
}
