//! A local relay credential is transport authority, never a user or Agent.
//! Bootstrap-controlled native startup pins the official relay and its queue
//! origin; private custody establishes this transport principal. Outer host
//! metadata correlates windows only after this authentication succeeds.
use crate::{AgentIsolationError, Database};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};

pub const NATIVE_AGENT_HOST_TOKEN_PREFIX: &str = "wc_nh_";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedNativeAgentHost {
    host_id: String,
    principal_key: String,
    generation: i64,
}

impl VerifiedNativeAgentHost {
    pub fn host_id(&self) -> &str {
        &self.host_id
    }
    pub fn principal_key(&self) -> &str {
        &self.principal_key
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }
}

// Intentionally neither Debug nor Serialize: the secret has one control-plane
// recipient and must not enter structured model output or diagnostic logs.
pub struct NativeAgentHostGrant {
    pub host_id: String,
    pub generation: i64,
    secret: String,
}

impl NativeAgentHostGrant {
    pub fn into_secret(self) -> String {
        self.secret
    }
}

fn unavailable(_: impl std::fmt::Display) -> AgentIsolationError {
    AgentIsolationError::new("native_host_unavailable")
}
fn invalid() -> AgentIsolationError {
    AgentIsolationError::new("native_host_invalid")
}
fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn token() -> String {
    let mut digest = Sha256::new();
    digest.update(b"webcodex.native-agent-host-secret.v1\0");
    // 366 OS-random bits; hashing leaves a full 256-bit secret.
    for _ in 0..3 {
        digest.update(uuid::Uuid::new_v4().as_bytes());
    }
    format!("{NATIVE_AGENT_HOST_TOKEN_PREFIX}{:x}", digest.finalize())
}

impl Database {
    pub(crate) fn init_native_agent_host_tables(&self) -> anyhow::Result<()> {
        self.conn.lock().map_err(unavailable)?.execute_batch(
            "CREATE TABLE IF NOT EXISTS wc_native_agent_hosts (
               host_id TEXT PRIMARY KEY, tunnel_id TEXT NOT NULL UNIQUE,
               principal_key TEXT NOT NULL UNIQUE, generation INTEGER NOT NULL,
               token_hash TEXT NOT NULL UNIQUE, binary_sha256 TEXT NOT NULL,
               enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
               created_at_unix_ms INTEGER NOT NULL, rotated_at_unix_ms INTEGER NOT NULL
             );",
        )?;
        Ok(())
    }

    /// Trusted local product control only, after checking the actual relay
    /// binary against the pinned digest. Model dispatch must never call this.
    /// A disabled registration cannot be revived by restart or re-registration.
    pub fn register_native_agent_host(
        &self,
        tunnel_id: &str,
        binary_sha256: &str,
    ) -> Result<NativeAgentHostGrant, AgentIsolationError> {
        if !tunnel_id
            .strip_prefix("tunnel_")
            .is_some_and(|v| hex(v, 32))
            || !hex(binary_sha256, 64)
        {
            return Err(AgentIsolationError::new("native_host_registration_invalid"));
        }
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let existing: Option<(String, i64, bool)> = tx
            .query_row(
                "SELECT host_id,generation,enabled FROM wc_native_agent_hosts WHERE tunnel_id=?1",
                [tunnel_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(unavailable)?;
        let secret = token();
        let now = chrono::Utc::now().timestamp_millis();
        let (host_id, generation) = if let Some((id, generation, enabled)) = existing {
            if !enabled {
                return Err(AgentIsolationError::new("native_host_revoked"));
            }
            let next = generation.checked_add(1).ok_or_else(invalid)?;
            tx.execute(
                "UPDATE wc_native_agent_hosts SET generation=?2,token_hash=?3,
                binary_sha256=?4,rotated_at_unix_ms=?5 WHERE host_id=?1",
                params![id, next, hash(&secret), binary_sha256, now],
            )
            .map_err(unavailable)?;
            (id, next)
        } else {
            let id = format!("wc_host_{}", uuid::Uuid::new_v4().simple());
            let principal = hash(&format!("webcodex.native-agent-host-principal.v1\0{id}"));
            tx.execute(
                "INSERT INTO wc_native_agent_hosts VALUES(?1,?2,?3,1,?4,?5,1,?6,?6)",
                params![id, tunnel_id, principal, hash(&secret), binary_sha256, now],
            )
            .map_err(unavailable)?;
            (id, 1)
        };
        tx.commit().map_err(unavailable)?;
        Ok(NativeAgentHostGrant {
            host_id,
            generation,
            secret,
        })
    }

    pub fn authenticate_native_agent_host(
        &self,
        credential: &str,
    ) -> Result<VerifiedNativeAgentHost, AgentIsolationError> {
        if !credential
            .strip_prefix(NATIVE_AGENT_HOST_TOKEN_PREFIX)
            .is_some_and(|v| hex(v, 64))
        {
            return Err(invalid());
        }
        let conn = self.conn.lock().map_err(unavailable)?;
        conn.query_row(
            "SELECT host_id,principal_key,generation FROM wc_native_agent_hosts
            WHERE token_hash=?1 AND enabled=1",
            [hash(credential)],
            |r| {
                Ok(VerifiedNativeAgentHost {
                    host_id: r.get(0)?,
                    principal_key: r.get(1)?,
                    generation: r.get(2)?,
                })
            },
        )
        .optional()
        .map_err(unavailable)?
        .ok_or_else(invalid)
    }

    pub fn validate_native_agent_host(
        &self,
        host: &VerifiedNativeAgentHost,
    ) -> Result<(), AgentIsolationError> {
        let valid: bool = self
            .conn
            .lock()
            .map_err(unavailable)?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM wc_native_agent_hosts WHERE host_id=?1 AND
             principal_key=?2 AND generation=?3 AND enabled=1)",
                params![host.host_id, host.principal_key, host.generation],
                |r| r.get(0),
            )
            .map_err(unavailable)?;
        if valid {
            Ok(())
        } else {
            Err(invalid())
        }
    }

    /// Stable principal for trusted control to issue an exact enrollment. This
    /// never selects an Agent, user, latest task, or host window implicitly.
    pub fn native_agent_host_principal(
        &self,
        host_id: &str,
    ) -> Result<String, AgentIsolationError> {
        self.conn
            .lock()
            .map_err(unavailable)?
            .query_row(
                "SELECT principal_key FROM wc_native_agent_hosts WHERE host_id=?1 AND enabled=1",
                [host_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(unavailable)?
            .ok_or_else(invalid)
    }

    pub fn revoke_native_agent_host(&self, host_id: &str) -> Result<(), AgentIsolationError> {
        let mut conn = self.conn.lock().map_err(unavailable)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let changed = tx
            .execute(
                "UPDATE wc_native_agent_hosts SET enabled=0 WHERE host_id=?1",
                [host_id],
            )
            .map_err(unavailable)?;
        if changed != 1 {
            return Err(invalid());
        }
        let principal: String = tx
            .query_row(
                "SELECT principal_key FROM wc_native_agent_hosts WHERE host_id=?1",
                [host_id],
                |r| r.get(0),
            )
            .map_err(unavailable)?;
        crate::agent_isolation::revoke_agent_enrollments_for_principal_on_connection(
            &tx, &principal,
        )?;
        // Host windows are lazily created by existing enrollment/host APIs.
        let windows: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='wc_agent_host_windows')",[],|r|r.get(0)).map_err(unavailable)?;
        if windows {
            tx.execute(
                "UPDATE wc_agent_host_windows SET enabled=0 WHERE principal_key=?1",
                [&principal],
            )
            .map_err(unavailable)?;
        }
        tx.commit().map_err(unavailable)
    }
}

#[cfg(test)]
#[path = "native_agent_host_tests.rs"]
mod tests;
