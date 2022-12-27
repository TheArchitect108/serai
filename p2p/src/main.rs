mod history;
mod state;
mod p2p;

use std::io;

use clap::{App, Arg};
use crate::p2p::P2PProcess;

// Test requirements:
// Establish connection between 2 users with provided connection info
// Each side will send their public key
// Each side with then send an message box encrypted message
// Successully decrypt encrypted message and display.
// Try to scale to many users.

// cargo run -- --config_dir ../p2p/config --name coordinator1
// cargo run -- --config_dir ../p2p/config --name coordinator2

#[tokio::main]
async fn main(){
  pretty_env_logger::init();

  let args = App::new("Serai P2P")
    .version("0.1.0")
    .author("Serai Team")
    .about("Serai P2P")
    .arg(
      Arg::with_name("config_dir")
        .short("cd")
        .long("config_dir")
        .help(
          "The path that the p2p messenger can find relevant config files.
                     Default: ./config/",
        )
        .takes_value(true)
        .default_value("../p2p/config/"),
    )
    .arg(
      Arg::with_name("name")
        .short("n")
        .long("name")
        .help("This name is used identify the coordinator docker address.")
        .takes_value(true)
        .default_value("base_coordinator"),
    )
    .get_matches();

  // Load Config / Coordinator Name
  let path_arg = args.value_of("config_dir").unwrap().to_owned();
  let name_arg = args.value_of("name").unwrap().to_owned();

  // Start P2P Process
  tokio::spawn(async move {
    let p2p_process = P2PProcess::new(name_arg.to_string(), path_arg.to_string());
    p2p_process.run().await;
  });

  // Hang on cli
  io::stdin().read_line(&mut String::new()).unwrap();
}

