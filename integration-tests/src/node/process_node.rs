use std::path::Path;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;
use std::{env, thread};

use log::error;
use rand::Rng;

use near_chain_configs::Genesis;
use near_crypto::{InMemorySigner, KeyType, Signer};
use near_primitives::types::AccountId;
use nearcore::config::NearConfig;

use crate::node::Node;
use crate::user::rpc_user::RpcUser;
use crate::user::User;
use actix::{Actor, System};
use futures::{FutureExt, TryFutureExt};
use near_jsonrpc_client::new_client;
use near_network::test_utils::WaitOrTimeout;

pub enum ProcessNodeState {
    Stopped,
    Running(Child),
}

pub struct ProcessNode {
    pub work_dir: String,
    pub config: NearConfig,
    pub state: ProcessNodeState,
    pub signer: Arc<InMemorySigner>,
}

impl Node for ProcessNode {
    fn genesis(&self) -> &Genesis {
        &self.config.genesis
    }

    fn account_id(&self) -> Option<AccountId> {
        match &self.config.validator_signer {
            Some(vs) => Some(vs.validator_id().clone()),
            None => None,
        }
    }

    fn start(&mut self) {
        match self.state {
            ProcessNodeState::Stopped => {
                std::env::set_var("ADVERSARY_CONSENT", "1");
                let child =
                    self.get_start_node_command().spawn().expect("start node command failed");
                self.state = ProcessNodeState::Running(child);
                let client_addr = format!("http://{}", self.config.rpc_addr().unwrap());
                thread::sleep(Duration::from_secs(3));
                near_actix_test_utils::run_actix(async move {
                    WaitOrTimeout::new(
                        Box::new(move |_| {
                            actix::spawn(
                                new_client(&client_addr)
                                    .status()
                                    .map_ok(|_| System::current().stop())
                                    .then(|_| futures::future::ready(())),
                            );
                        }),
                        1000,
                        30000,
                    )
                    .start();
                });
            }
            ProcessNodeState::Running(_) => panic!("Node is already running"),
        }
    }

    fn kill(&mut self) {
        match self.state {
            ProcessNodeState::Running(ref mut child) => {
                child.kill().expect("kill failed");
                thread::sleep(Duration::from_secs(1));
                self.state = ProcessNodeState::Stopped;
            }
            ProcessNodeState::Stopped => panic!("Invalid state"),
        }
    }

    fn signer(&self) -> Arc<dyn Signer> {
        self.signer.clone()
    }

    fn is_running(&self) -> bool {
        match self.state {
            ProcessNodeState::Stopped => false,
            ProcessNodeState::Running(_) => true,
        }
    }

    fn user(&self) -> Box<dyn User> {
        let account_id = self.signer.account_id.clone();
        Box::new(RpcUser::new(self.config.rpc_addr().unwrap(), account_id, self.signer.clone()))
    }

    fn as_process_ref(&self) -> &ProcessNode {
        self
    }

    fn as_process_mut(&mut self) -> &mut ProcessNode {
        self
    }
}

impl ProcessNode {
    /// Side effect: reset_storage
    pub fn new(config: NearConfig) -> ProcessNode {
        let mut rng = rand::thread_rng();
        let work_dir = format!(
            "{}/process_node_{}",
            env::temp_dir().as_path().to_str().unwrap(),
            rng.gen::<u64>()
        );
        let signer = Arc::new(InMemorySigner::from_seed(
            config.validator_signer.as_ref().unwrap().validator_id().clone(),
            KeyType::ED25519,
            config.validator_signer.as_ref().unwrap().validator_id().as_ref(),
        ));
        let result = ProcessNode { config, work_dir, state: ProcessNodeState::Stopped, signer };
        result.reset_storage();
        result
    }

    /// Clear storage directory and run keygen
    pub fn reset_storage(&self) {
        Command::new("rm").args(&["-r", &self.work_dir]).spawn().unwrap().wait().unwrap();
        self.config.save_to_dir(Path::new(&self.work_dir));
    }

    /// Side effect: writes chain spec file
    pub fn get_start_node_command(&self) -> Command {
        if let Err(_) = std::env::var("NIGHTLY_RUNNER") {
            let mut command = Command::new("cargo");
            command.args(&["run", "-p", "neard"]);
            #[cfg(feature = "nightly_protocol")]
            command.args(&["--features", "nightly_protocol"]);
            #[cfg(feature = "nightly_protocol_features")]
            command.args(&["--features", "nightly_protocol_features"]);
            command.args(&["--bin", "neard", "--", "--home", &self.work_dir, "run"]);
            command
        } else {
            let mut command = Command::new("target/debug/neard");
            command.args(&["--home", &self.work_dir, "run"]);
            command
        }
    }
}

impl Drop for ProcessNode {
    fn drop(&mut self) {
        match self.state {
            ProcessNodeState::Running(ref mut child) => {
                let _ = child.kill().map_err(|_| error!("child process died"));
                std::fs::remove_dir_all(self.work_dir.clone()).unwrap();
            }
            ProcessNodeState::Stopped => {}
        }
    }
}
