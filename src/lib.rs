use anyhow::anyhow;
use exogress_common::client_core::Client;
use exogress_common::entities::{
    AccessKeyId, AccountName, LabelName, LabelValue, ProfileName, ProjectName, SmolStr,
};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures::channel::{mpsc, oneshot};
use hashbrown::HashMap;
use log::info;
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use tokio::runtime::Runtime;
use trust_dns_resolver::{TokioAsyncResolver, TokioHandle};

const CRATE_VERSION: &'static str = env!("CARGO_PKG_VERSION");

create_exception!(exogress, ExogressError, PyException);

fn extract_key<'a, 'b, T: FromPyObject<'a>>(
    kwds: &'a PyDict,
    key: &'b str,
) -> Result<Option<T>, pyo3::PyErr> {
    match kwds.get_item(key) {
        Some(item) => Ok(Some(item.extract::<'a, T>().map_err(|e| {
            ExogressError::new_err(format!("bad {} supplied: {}", key, e))
        })?)),
        None => Ok(None),
    }
}

#[pyclass]
struct Instance {
    client: parking_lot::Mutex<Option<exogress_common::client_core::Client>>,
    reload_config_tx: parking_lot::Mutex<UnboundedSender<()>>,
    reload_config_rx: parking_lot::Mutex<Option<UnboundedReceiver<()>>>,
    stop_tx: parking_lot::Mutex<Option<oneshot::Sender<()>>>,
    stop_rx: parking_lot::Mutex<Option<oneshot::Receiver<()>>>,
}

#[pymethods]
impl Instance {
    #[new]
    #[args(kwds = "**")]
    pub fn new(kwds: Option<&PyDict>) -> PyResult<Self> {
        let kwds = kwds.ok_or_else(|| ExogressError::new_err("no data supplied"))?;

        let access_key_id = extract_key::<String>(&kwds, "access_key_id")?
            .ok_or_else(|| ExogressError::new_err("access_key_id was not provided"))?
            .parse::<AccessKeyId>()
            .map_err(|e| ExogressError::new_err(e.to_string()))?;

        let secret_access_key: String = extract_key::<String>(&kwds, "secret_access_key")?
            .ok_or_else(|| ExogressError::new_err("secret_access_key was not provided"))?;

        let account = extract_key::<String>(&kwds, "account")?
            .ok_or_else(|| ExogressError::new_err("account was not provided"))?
            .parse::<AccountName>()
            .map_err(|e| ExogressError::new_err(e.to_string()))?;

        let project = extract_key::<String>(&kwds, "project")?
            .ok_or_else(|| ExogressError::new_err("project was not provided"))?
            .parse::<ProjectName>()
            .map_err(|e| ExogressError::new_err(e.to_string()))?;

        let maybe_config_path = extract_key::<String>(&kwds, "config_path")?;
        let labels = extract_key::<std::collections::HashMap<String, String>>(&kwds, "labels")?
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| {
                Ok::<_, anyhow::Error>((
                    k.parse::<LabelName>()?,
                    v.parse::<LabelValue>()
                        .map_err(|()| anyhow!("bad label value"))?,
                ))
            })
            .collect::<Result<HashMap<LabelName, LabelValue>, anyhow::Error>>()
            .map_err(|e| ExogressError::new_err(e.to_string()))?;

        let watch_config = extract_key::<bool>(&kwds, "watch_config")?.unwrap_or(true);

        let profile = if let Some(profile) = extract_key::<String>(&kwds, "profile")? {
            Some(
                profile
                    .parse::<ProfileName>()
                    .map_err(|e| ExogressError::new_err(e.to_string()))?,
            )
        } else {
            None
        };

        let mut client_builder = Client::builder();

        if let Some(config_path) = maybe_config_path {
            client_builder.config_path(config_path);
        }

        let client = client_builder
            .access_key_id(access_key_id.clone())
            .secret_access_key(secret_access_key.clone())
            .account(account.clone())
            .project(project.clone())
            .watch_config(watch_config)
            .profile(profile)
            .labels(labels)
            .additional_connection_params({
                let mut map = HashMap::<SmolStr, SmolStr>::new();
                map.insert("client".into(), "python".into());
                map.insert("wrapper_version".into(), CRATE_VERSION.into());
                map
            })
            .build()
            .map_err(|e| ExogressError::new_err(e.to_string()))?;

        let (reload_config_tx, reload_config_rx) = mpsc::unbounded();
        let (stop_tx, stop_rx) = oneshot::channel();

        Ok(Instance {
            client: parking_lot::Mutex::new(Some(client)),
            reload_config_tx: parking_lot::Mutex::new(reload_config_tx),
            reload_config_rx: parking_lot::Mutex::new(Some(reload_config_rx)),
            stop_tx: parking_lot::Mutex::new(Some(stop_tx)),
            stop_rx: parking_lot::Mutex::new(Some(stop_rx)),
        })
    }

    fn spawn(&self, py: Python) -> PyResult<()> {
        py.allow_threads(move || {
            let rt = Runtime::new().map_err(|e| ExogressError::new_err(e.to_string()))?;

            let resolver = TokioAsyncResolver::from_system_conf(TokioHandle)
                .map_err(|e| ExogressError::new_err(e.to_string()))?;

            let reload_config_rx = self
                .reload_config_rx
                .lock()
                .take()
                .ok_or_else(|| ExogressError::new_err("instance has already been spawned"))?;
            let reload_config_tx = self.reload_config_tx.lock().clone();

            let stop_rx = self
                .stop_rx
                .lock()
                .take()
                .ok_or_else(|| ExogressError::new_err("instance has already been spawned"))?;

            if let Some(client) = self.client.lock().take() {
                rt.block_on(async move {
                    let spawn = client.spawn(reload_config_tx, reload_config_rx, resolver);

                    tokio::select! {
                        r = spawn => {
                            if let Err(e) = r {
                                return Err(ExogressError::new_err(e.to_string()));
                            }
                        },
                        _ = stop_rx => {
                            info!("stop exogress instance by request");
                        }
                    }

                    Ok(())
                })?;

                Ok(())
            } else {
                return Err(ExogressError::new_err(
                    "cannot start already stopped instance",
                ));
            }
        })
    }

    fn reload(&self) -> PyResult<()> {
        self.reload_config_tx
            .lock()
            .unbounded_send(())
            .map_err(|e| ExogressError::new_err(format!("failed to send reload request: {}", e)))
    }

    fn stop(&self) -> PyResult<()> {
        self.stop_tx
            .lock()
            .take()
            .ok_or_else(|| ExogressError::new_err(format!("instance already stopped")))?
            .send(())
            .map_err(|_| ExogressError::new_err(format!("failed to send reload request")))
    }
}

/// A Python module implemented in Rust.
#[pymodule]
fn exogress(py: Python, m: &PyModule) -> PyResult<()> {
    pyo3_log::init();

    m.add_class::<Instance>()?;

    m.add("ExogressError", py.get_type::<ExogressError>())?;

    Ok(())
}
