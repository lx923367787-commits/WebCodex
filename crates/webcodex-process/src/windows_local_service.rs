//! Exact AppContainer/TCP/127.0.0.1/port permission for local project services.
//! Persistent default-deny filters survive controller loss. The narrow permits
//! are dynamic: losing their owning handle removes permission, not isolation.
//! No global firewall policy or AppContainer exemption list is changed.
use crate::windows_lpac::AppContainerSid;
use std::ptr::{null, null_mut};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER};
use windows_sys::Win32::Foundation::{FWP_E_FILTER_NOT_FOUND, FWP_E_SUBLAYER_NOT_FOUND, HANDLE};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCPTABLE_OWNER_PID, MIB_TCP_STATE_LISTEN, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::*;
use windows_sys::Win32::Networking::WinSock::AF_INET;
use windows_sys::Win32::Security::{EqualSid, PSID};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Rpc::RPC_C_AUTHN_WINNT;
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

fn same_guid(a: GUID, b: GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}
fn check(code: u32) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!("program_network_policy_os:{code}"))
    }
}
fn key(identity: &str, index: u8) -> Result<GUID, String> {
    if identity.len() != 64
        || !identity
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("program_network_identity_invalid".into());
    }
    let n = u128::from_str_radix(&identity[..32], 16)
        .map_err(|_| "program_network_identity_invalid")?;
    Ok(GUID::from_u128(n ^ u128::from(index)))
}
struct Engine(HANDLE);
// WFP engine handles can move between threads; ownership remains unique.
unsafe impl Send for Engine {}
impl Engine {
    fn open(dynamic: bool) -> Result<Self, String> {
        let mut handle = null_mut();
        let session = FWPM_SESSION0 {
            flags: if dynamic {
                FWPM_SESSION_FLAG_DYNAMIC
            } else {
                0
            },
            txnWaitTimeoutInMSec: 5000,
            ..Default::default()
        };
        check(unsafe {
            FwpmEngineOpen0(null(), RPC_C_AUTHN_WINNT, null(), &session, &mut handle)
        })?;
        Ok(Self(handle))
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        unsafe {
            FwpmEngineClose0(self.0);
        }
    }
}
fn package(sid: PSID) -> FWPM_FILTER_CONDITION0 {
    FWPM_FILTER_CONDITION0 {
        fieldKey: FWPM_CONDITION_ALE_PACKAGE_ID,
        matchType: FWP_MATCH_EQUAL,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_SID,
            Anonymous: FWP_CONDITION_VALUE0_0 { sid: sid.cast() },
        },
    }
}
fn address(field: GUID) -> FWPM_FILTER_CONDITION0 {
    FWPM_FILTER_CONDITION0 {
        fieldKey: field,
        matchType: FWP_MATCH_EQUAL,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_UINT32,
            Anonymous: FWP_CONDITION_VALUE0_0 { uint32: 0x7f000001 },
        },
    }
}
fn port(field: GUID, value: u16) -> FWPM_FILTER_CONDITION0 {
    FWPM_FILTER_CONDITION0 {
        fieldKey: field,
        matchType: FWP_MATCH_EQUAL,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_UINT16,
            Anonymous: FWP_CONDITION_VALUE0_0 { uint16: value },
        },
    }
}
fn tcp() -> FWPM_FILTER_CONDITION0 {
    FWPM_FILTER_CONDITION0 {
        fieldKey: FWPM_CONDITION_IP_PROTOCOL,
        matchType: FWP_MATCH_EQUAL,
        conditionValue: FWP_CONDITION_VALUE0 {
            r#type: FWP_UINT8,
            Anonymous: FWP_CONDITION_VALUE0_0 { uint8: 6 },
        },
    }
}
const LAYERS: [GUID; 4] = [
    FWPM_LAYER_ALE_AUTH_CONNECT_V4,
    FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V4,
    FWPM_LAYER_ALE_AUTH_CONNECT_V6,
    FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V6,
];
fn add_filter(
    engine: &Engine,
    identity: &str,
    index: u8,
    layer: GUID,
    sid: PSID,
    service_port: Option<u16>,
) -> Result<(), String> {
    let mut conditions = vec![package(sid)];
    if let Some(value) = service_port {
        conditions.extend([
            address(FWPM_CONDITION_IP_LOCAL_ADDRESS),
            address(FWPM_CONDITION_IP_REMOTE_ADDRESS),
            tcp(),
            port(
                if index == 5 {
                    FWPM_CONDITION_IP_REMOTE_PORT
                } else {
                    FWPM_CONDITION_IP_LOCAL_PORT
                },
                value,
            ),
        ]);
    }
    let mut name = "WebCodex isolated local service\0"
        .encode_utf16()
        .collect::<Vec<_>>();
    let filter = FWPM_FILTER0 {
        filterKey: key(identity, index)?,
        layerKey: layer,
        subLayerKey: key(identity, 16)?,
        displayData: FWPM_DISPLAY_DATA0 {
            name: name.as_mut_ptr(),
            description: null_mut(),
        },
        flags: if service_port.is_some() {
            FWPM_FILTER_FLAG_CLEAR_ACTION_RIGHT
        } else {
            FWPM_FILTER_FLAG_PERSISTENT
        },
        weight: FWP_VALUE0 {
            r#type: FWP_UINT8,
            Anonymous: FWP_VALUE0_0 {
                uint8: if service_port.is_some() { 15 } else { 1 },
            },
        },
        numFilterConditions: conditions.len() as u32,
        filterCondition: conditions.as_mut_ptr(),
        action: FWPM_ACTION0 {
            r#type: if service_port.is_some() {
                FWP_ACTION_PERMIT
            } else {
                FWP_ACTION_BLOCK
            },
            ..Default::default()
        },
        ..Default::default()
    };
    check(unsafe { FwpmFilterAdd0(engine.0, &filter, null_mut(), null_mut()) })
}

pub struct LocalServiceNetwork {
    permit: Option<Engine>,
    identity: String,
    port: u16,
}
impl std::fmt::Debug for LocalServiceNetwork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalServiceNetwork([exact package/port])")
    }
}
impl LocalServiceNetwork {
    pub fn cleanup_complete(identity: &str) -> Result<bool, String> {
        let engine = Engine::open(false)?;
        for index in 1..=6 {
            let mut filter = null_mut();
            let code =
                unsafe { FwpmFilterGetByKey0(engine.0, &key(identity, index)?, &mut filter) };
            if code == FWP_E_FILTER_NOT_FOUND as u32 {
                continue;
            }
            check(code)?;
            unsafe {
                FwpmFreeMemory0((&mut filter as *mut *mut FWPM_FILTER0).cast());
            }
            return Ok(false);
        }
        let mut layer = null_mut();
        let code = unsafe { FwpmSubLayerGetByKey0(engine.0, &key(identity, 16)?, &mut layer) };
        if code == FWP_E_SUBLAYER_NOT_FOUND as u32 {
            return Ok(true);
        }
        check(code)?;
        unsafe {
            FwpmFreeMemory0((&mut layer as *mut *mut FWPM_SUBLAYER0).cast());
        }
        Ok(false)
    }
    /// The caller must persist this identity as cleanup intent before calling.
    /// Existing objects are not adopted or overwritten, even after uncertainty.
    pub fn install(
        sid: &AppContainerSid,
        identity: &str,
        service_port: u16,
    ) -> Result<Self, String> {
        if service_port < 1024 {
            return Err("program_network_port_invalid".into());
        }
        let engine = Engine::open(false)?;
        let mut name = "WebCodex program network boundary\0"
            .encode_utf16()
            .collect::<Vec<_>>();
        let sublayer = FWPM_SUBLAYER0 {
            subLayerKey: key(identity, 16)?,
            displayData: FWPM_DISPLAY_DATA0 {
                name: name.as_mut_ptr(),
                description: null_mut(),
            },
            flags: FWPM_SUBLAYER_FLAG_PERSISTENT,
            weight: 65535,
            ..Default::default()
        };
        check(unsafe { FwpmTransactionBegin0(engine.0, 0) })?;
        let installed = (|| {
            check(unsafe { FwpmSubLayerAdd0(engine.0, &sublayer, null_mut()) })?;
            for (i, layer) in LAYERS.iter().enumerate() {
                add_filter(&engine, identity, i as u8 + 1, *layer, sid.as_ptr(), None)?;
            }
            check(unsafe { FwpmTransactionCommit0(engine.0) })
        })();
        if let Err(error) = installed {
            unsafe {
                FwpmTransactionAbort0(engine.0);
            };
            return Err(error);
        }
        let permit = Engine::open(true)?;
        check(unsafe { FwpmTransactionBegin0(permit.0, 0) })?;
        let enabled = (|| {
            for (i, layer) in LAYERS[..2].iter().enumerate() {
                add_filter(
                    &permit,
                    identity,
                    i as u8 + 5,
                    *layer,
                    sid.as_ptr(),
                    Some(service_port),
                )?;
            }
            check(unsafe { FwpmTransactionCommit0(permit.0) })
        })();
        if let Err(error) = enabled {
            unsafe {
                FwpmTransactionAbort0(permit.0);
            };
            return Err(error);
        }
        Ok(Self {
            permit: Some(permit),
            identity: identity.into(),
            port: service_port,
        })
    }
    pub fn mode(&self) -> crate::windows_lpac::LpacNetwork {
        crate::windows_lpac::LpacNetwork::LocalService {
            port: self.port,
            policy_key: u128::from_str_radix(&self.identity[..32], 16)
                .expect("validated identity")
                .to_be_bytes(),
        }
    }
    /// Call only after complete owned-tree stop proof. An ordinary Drop revokes
    /// dynamic permission but intentionally leaves persistent default-deny.
    pub fn remove_after_stop(mut self) -> Result<(), String> {
        self.permit.take();
        Self::remove_stopped(&self.identity)
    }
    pub fn remove_stopped(identity: &str) -> Result<(), String> {
        let sid = AppContainerSid::derive(identity).map_err(|e| e.to_string())?;
        let engine = Engine::open(false)?;
        check(unsafe { FwpmTransactionBegin0(engine.0, 0) })?;
        let removed = (|| {
            for index in 1..=4 {
                let mut filter = null_mut();
                let code =
                    unsafe { FwpmFilterGetByKey0(engine.0, &key(identity, index)?, &mut filter) };
                if code == FWP_E_FILTER_NOT_FOUND as u32 {
                    continue;
                }
                check(code)?;
                let owned = unsafe {
                    let f = &*filter;
                    f.action.r#type == FWP_ACTION_BLOCK
                        && f.numFilterConditions == 1
                        && same_guid((*f.filterCondition).fieldKey, FWPM_CONDITION_ALE_PACKAGE_ID)
                        && (*f.filterCondition).conditionValue.r#type == FWP_SID
                        && EqualSid(
                            (*f.filterCondition).conditionValue.Anonymous.sid.cast(),
                            sid.as_ptr(),
                        ) != 0
                };
                unsafe {
                    FwpmFreeMemory0((&mut filter as *mut *mut FWPM_FILTER0).cast());
                }
                if !owned {
                    return Err("program_network_cleanup_identity_mismatch".into());
                }
                check(unsafe { FwpmFilterDeleteByKey0(engine.0, &key(identity, index)?) })?;
            }
            let code = unsafe { FwpmSubLayerDeleteByKey0(engine.0, &key(identity, 16)?) };
            if code != FWP_E_SUBLAYER_NOT_FOUND as u32 {
                check(code)?;
            }
            check(unsafe { FwpmTransactionCommit0(engine.0) })
        })();
        if removed.is_err() {
            unsafe {
                FwpmTransactionAbort0(engine.0);
            }
        }
        removed
    }
}

pub(crate) fn verify_permission(
    sid: &AppContainerSid,
    policy_key: [u8; 16],
    port: u16,
) -> Result<(), String> {
    let identity = format!("{:032x}{}", u128::from_be_bytes(policy_key), "0".repeat(32));
    let engine = Engine::open(false)?;
    for index in 1..=6 {
        let mut filter = null_mut();
        check(unsafe { FwpmFilterGetByKey0(engine.0, &key(&identity, index)?, &mut filter) })?;
        let owned = unsafe {
            let f = &*filter;
            if f.numFilterConditions == 0
                || f.numFilterConditions > 8
                || f.filterCondition.is_null()
            {
                FwpmFreeMemory0((&mut filter as *mut *mut FWPM_FILTER0).cast());
                return Err("program_network_policy_changed".into());
            }
            let conditions =
                std::slice::from_raw_parts(f.filterCondition, f.numFilterConditions as usize);
            let package_ok = conditions.first().is_some_and(|c| {
                same_guid(c.fieldKey, FWPM_CONDITION_ALE_PACKAGE_ID)
                    && c.matchType == FWP_MATCH_EQUAL
                    && c.conditionValue.r#type == FWP_SID
                    && EqualSid(c.conditionValue.Anonymous.sid.cast(), sid.as_ptr()) != 0
            });
            let permit = index > 4;
            let layer = LAYERS[if permit {
                index as usize - 5
            } else {
                index as usize - 1
            }];
            same_guid(f.layerKey, layer)
                && same_guid(f.subLayerKey, key(&identity, 16)?)
                && package_ok
                && if permit {
                    f.action.r#type == FWP_ACTION_PERMIT
                        && f.flags & FWPM_FILTER_FLAG_CLEAR_ACTION_RIGHT != 0
                        && conditions.len() == 5
                        && [1usize, 2].iter().all(|i| {
                            conditions[*i].conditionValue.r#type == FWP_UINT32
                                && conditions[*i].conditionValue.Anonymous.uint32 == 0x7f000001
                                && conditions[*i].matchType == FWP_MATCH_EQUAL
                        })
                        && same_guid(conditions[1].fieldKey, FWPM_CONDITION_IP_LOCAL_ADDRESS)
                        && same_guid(conditions[2].fieldKey, FWPM_CONDITION_IP_REMOTE_ADDRESS)
                        && same_guid(conditions[3].fieldKey, FWPM_CONDITION_IP_PROTOCOL)
                        && conditions[3].conditionValue.r#type == FWP_UINT8
                        && conditions[3].conditionValue.Anonymous.uint8 == 6
                        && same_guid(
                            conditions[4].fieldKey,
                            if index == 5 {
                                FWPM_CONDITION_IP_REMOTE_PORT
                            } else {
                                FWPM_CONDITION_IP_LOCAL_PORT
                            },
                        )
                        && conditions[4].conditionValue.r#type == FWP_UINT16
                        && conditions[4].conditionValue.Anonymous.uint16 == port
                } else {
                    f.action.r#type == FWP_ACTION_BLOCK
                        && conditions.len() == 1
                        && f.flags & FWPM_FILTER_FLAG_PERSISTENT != 0
                }
        };
        unsafe {
            FwpmFreeMemory0((&mut filter as *mut *mut FWPM_FILTER0).cast());
        }
        if !owned {
            return Err("program_network_policy_changed".into());
        }
    }
    Ok(())
}

/// Observe kernel TCP listeners and prove their current process belongs to the
/// retained Job handle. PID text or a workload's stdout is never port evidence.
pub(crate) fn owned_listeners(job: HANDLE) -> std::io::Result<Vec<(u32, u16)>> {
    let mut bytes = 0u32;
    let first = unsafe {
        GetExtendedTcpTable(
            null_mut(),
            &mut bytes,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if first != 0 && first != ERROR_INSUFFICIENT_BUFFER {
        return Err(std::io::Error::from_raw_os_error(first as i32));
    }
    for _ in 0..3 {
        if bytes < 4 || bytes > 16 * 1024 * 1024 {
            return Err(std::io::Error::other("TCP observation bound"));
        }
        let mut buffer = vec![0usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
        let status = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr().cast(),
                &mut bytes,
                0,
                AF_INET as u32,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status as i32));
        }
        let table = unsafe { &*buffer.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>() };
        let count = table.dwNumEntries as usize;
        let row_size = std::mem::size_of_val(&table.table[0]);
        if count > (bytes as usize).saturating_sub(4) / row_size {
            return Err(std::io::Error::other("TCP observation invalid"));
        }
        let rows = unsafe { std::slice::from_raw_parts(table.table.as_ptr(), count) };
        let mut found = Vec::new();
        for row in rows {
            if row.dwState != MIB_TCP_STATE_LISTEN as u32
                || row.dwLocalAddr.to_ne_bytes() != [127, 0, 0, 1]
            {
                continue;
            }
            let process =
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, row.dwOwningPid) };
            if process.is_null() {
                continue;
            }
            let mut owned = 0;
            let ok = unsafe { IsProcessInJob(process, job, &mut owned) };
            unsafe {
                CloseHandle(process);
            }
            if ok != 0 && owned != 0 {
                found.push((row.dwOwningPid, u16::from_be(row.dwLocalPort as u16)));
            }
        }
        found.sort_unstable();
        found.dedup();
        return Ok(found);
    }
    Err(std::io::Error::other("TCP observation changed repeatedly"))
}
