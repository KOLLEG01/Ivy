//! Late binding without retaining a service through its own callbacks.
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_sdk::service::{ServiceClient, ServiceConnection};
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone, Default)]
pub struct ServiceLink(Arc<Mutex<Weak<ServiceClient>>>);
impl ServiceLink {
    pub fn bind(&self, service: &Arc<ServiceClient>) -> IvyResult<()> {
        *self.0.lock().map_err(|_| IvyError::internal())? = Arc::downgrade(service);
        Ok(())
    }
    pub fn connection(&self) -> IvyResult<ServiceConnection> {
        self.0
            .lock()
            .map_err(|_| IvyError::internal())?
            .upgrade()
            .ok_or_else(|| {
                IvyError::new(
                    "service_not_ready",
                    "The service has not reconciled its current Hive generation.",
                )
            })?
            .connection()
    }
    pub fn ready(&self) -> bool {
        self.connection().is_ok()
    }
    pub fn notify(&self, name: &str, value: IvyJson) -> IvyResult<()> {
        if let Ok(connection) = self.connection() {
            connection.notification("agent".into(), name.into(), "1.0.0".into(), value)?;
        }
        Ok(())
    }
}
