//! Late binding without retaining a service through its own callbacks.
use crate::backend_readiness::BackendReadiness;
use ivy_contracts::{IvyError, IvyJson, IvyResult, native_backend::BackendIdentity};
use ivy_sdk::service::{ServiceClient, ServiceConnection};
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone, Default)]
pub struct ServiceLink {
    service: Arc<Mutex<Weak<ServiceClient>>>,
    backend: Option<BackendIdentity>,
    readiness: Option<BackendReadiness>,
}
impl ServiceLink {
    pub fn bind(&self, service: &Arc<ServiceClient>) -> IvyResult<()> {
        *self.service.lock().map_err(|_| IvyError::internal())? = Arc::downgrade(service);
        Ok(())
    }
    pub fn connection(&self) -> IvyResult<ServiceConnection> {
        self.service
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
        self.readiness.as_ref().is_none_or(BackendReadiness::ready) && self.connection().is_ok()
    }
    pub(crate) fn scoped(
        &self,
        backend: Option<BackendIdentity>,
        readiness: BackendReadiness,
    ) -> Self {
        Self {
            service: self.service.clone(),
            backend,
            readiness: Some(readiness),
        }
    }
    pub fn notify(&self, name: &str, mut value: IvyJson) -> IvyResult<()> {
        if self.readiness.as_ref().is_some_and(|gate| !gate.ready()) {
            return Ok(());
        }
        if let Some(backend) = &self.backend {
            value.insert("backend", backend.json());
        }
        if let Ok(connection) = self.connection() {
            connection.notification("agent".into(), name.into(), "1.0.0".into(), value)?;
        }
        Ok(())
    }
}
