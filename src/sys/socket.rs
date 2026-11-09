//! Listening-socket discovery for a process.
//!
//! Sources: `proc_pidinfo(PROC_PIDLISTFDS)` and
//! `proc_pidfdinfo(PROC_PIDFDSOCKETINFO)`. The decoding step is a pure
//! function over a `socket_fdinfo`, so it can be exercised with synthetic
//! structs without a live process.

use std::mem::size_of;
use std::ptr;

use super::SysError;

const PROX_FDTYPE_SOCKET: u32 = 2;
const PROC_PIDLISTFDS: libc::c_int = 1;
const PROC_PIDFDSOCKETINFO: libc::c_int = 3;

const SOCKINFO_TCP: i32 = 2;
const TSI_S_LISTEN: i32 = 1;

const INI_IPV4: u8 = 0x1;
const INI_IPV6: u8 = 0x2;

extern "C" {
    fn proc_pidinfo(
        pid: libc::c_int,
        flavor: libc::c_int,
        arg: u64,
        buffer: *mut libc::c_void,
        buffersize: libc::c_int,
    ) -> libc::c_int;
    fn proc_pidfdinfo(
        pid: libc::c_int,
        fd: libc::c_int,
        flavor: libc::c_int,
        buffer: *mut libc::c_void,
        buffersize: libc::c_int,
    ) -> libc::c_int;
}

/// `struct vinfo_stat` from `sys/proc_info.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VinfoStat {
    pub vst_dev: u32,
    pub vst_mode: u16,
    pub vst_nlink: u16,
    pub vst_ino: u64,
    pub vst_uid: u32,
    pub vst_gid: u32,
    pub vst_atime: i64,
    pub vst_atimensec: i64,
    pub vst_mtime: i64,
    pub vst_mtimensec: i64,
    pub vst_ctime: i64,
    pub vst_ctimensec: i64,
    pub vst_birthtime: i64,
    pub vst_birthtimensec: i64,
    pub vst_size: i64,
    pub vst_blocks: i64,
    pub vst_blksize: i32,
    pub vst_flags: u32,
    pub vst_gen: u32,
    pub vst_rdev: u32,
    pub vst_qspare: [i64; 2],
}

/// `struct proc_fileinfo` from `sys/proc_info.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcFileInfo {
    pub fi_openflags: u32,
    pub fi_status: u32,
    pub fi_offset: i64,
    pub fi_type: i32,
    pub fi_guardflags: u32,
}

/// `struct proc_fdinfo` from `sys/proc_info.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcFdInfo {
    pub proc_fd: i32,
    pub proc_fdtype: u32,
}

/// `struct sockbuf_info`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SockbufInfo {
    pub sbi_cc: u32,
    pub sbi_hiwat: u32,
    pub sbi_mbcnt: u32,
    pub sbi_mbmax: u32,
    pub sbi_lowat: u32,
    pub sbi_flags: i16,
    pub sbi_timeo: i16,
}

/// `struct insi_v6` from `struct in_sockinfo`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct InSockInfoV6 {
    pub in6_hlim: u8,
    pub in6_cksum: i32,
    pub in6_ifindex: u16,
    pub in6_hops: i16,
}

/// `struct in_sockinfo` from `sys/proc_info.h`.
///
/// The `insi_faddr` and `insi_laddr` unions are kept as raw 16-byte blocks:
/// the union is at most `struct in6_addr` (16 bytes) and both members share
/// the same layout for our purposes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct InSockInfo {
    pub insi_fport: i32,
    pub insi_lport: i32,
    pub insi_gencnt: u64,
    pub insi_flags: u32,
    pub insi_flow: u32,
    pub insi_vflag: u8,
    pub insi_ip_ttl: u8,
    pub rfu_1: u32,
    pub insi_faddr: [u8; 16],
    pub insi_laddr: [u8; 16],
    pub insi_v4_tos: u8,
    pub insi_v6: InSockInfoV6,
}

/// `struct tcp_sockinfo` from `sys/proc_info.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TcpSockInfo {
    pub tcpsi_ini: InSockInfo,
    pub tcpsi_state: i32,
    pub tcpsi_timer: [i32; 4],
    pub tcpsi_mss: i32,
    pub tcpsi_flags: u32,
    pub rfu_1: u32,
    pub tcpsi_tp: u64,
}

/// The TCP member of the `soi_proto` union.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SocketProto {
    pub pri_tcp: TcpSockInfo,
}

/// `struct socket_info` from `sys/proc_info.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SocketInfo {
    pub soi_stat: VinfoStat,
    pub soi_so: u64,
    pub soi_pcb: u64,
    pub soi_type: i32,
    pub soi_protocol: i32,
    pub soi_family: i32,
    pub soi_options: i16,
    pub soi_linger: i16,
    pub soi_state: i16,
    pub soi_qlen: i16,
    pub soi_incqlen: i16,
    pub soi_qlimit: i16,
    pub soi_timeo: i16,
    pub soi_error: u16,
    pub soi_oobmark: u32,
    pub soi_rcv: SockbufInfo,
    pub soi_snd: SockbufInfo,
    pub soi_kind: i32,
    pub rfu_1: u32,
    pub soi_proto: SocketProto,
}

/// `struct socket_fdinfo` from `sys/proc_info.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SocketFdInfo {
    pub pfi: ProcFileInfo,
    pub psi: SocketInfo,
}

/// A decoded listening endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenAddr {
    pub port: u16,
}

/// Decode one `socket_fdinfo`, keeping only TCP sockets in the LISTEN state
/// bound to a loopback or wildcard address.
pub fn decode_socket_info(info: &SocketFdInfo) -> Option<ListenAddr> {
    if info.psi.soi_kind != SOCKINFO_TCP {
        return None;
    }

    let tcp = &info.psi.soi_proto.pri_tcp;
    if tcp.tcpsi_state != TSI_S_LISTEN {
        return None;
    }

    // insi_lport is stored in network byte order.
    let port = u16::from_be(tcp.tcpsi_ini.insi_lport as u32 as u16);
    if port == 0 {
        return None;
    }

    if !address_allowed(tcp.tcpsi_ini.insi_vflag, &tcp.tcpsi_ini.insi_laddr) {
        return None;
    }

    Some(ListenAddr { port })
}

fn address_allowed(vflag: u8, address: &[u8; 16]) -> bool {
    if vflag & INI_IPV6 != 0 {
        let wildcard = address.iter().all(|byte| *byte == 0);
        let loopback = address[..15].iter().all(|byte| *byte == 0) && address[15] == 1;
        return wildcard || loopback;
    }

    // The IPv4 address sits in the last four bytes of the shared union; the
    // three pad words before it must be zero.
    if vflag & INI_IPV4 != 0 || vflag == 0 {
        if address[..12].iter().any(|byte| *byte != 0) {
            return false;
        }
        let wildcard = address[12..16].iter().all(|byte| *byte == 0);
        let loopback = address[12] == 127;
        return wildcard || loopback;
    }

    false
}

/// List the ports of the loopback/wildcard TCP sockets in `LISTEN` state.
///
/// A permission error is returned to the caller, which reports the instance
/// with an unknown port rather than dropping it.
pub fn list_listening_ports(pid: i32) -> Result<Vec<u16>, SysError> {
    // SAFETY: a null buffer asks only for the required byte count.
    let required = unsafe { proc_pidinfo(pid, PROC_PIDLISTFDS, 0, ptr::null_mut(), 0) };
    if required < 0 {
        return Err(SysError::last_os_error("proc_pidinfo(PROC_PIDLISTFDS)"));
    }
    if required == 0 {
        return Ok(Vec::new());
    }

    let entry_size = size_of::<ProcFdInfo>();
    let capacity = required as usize + entry_size * 4;
    let mut buffer = vec![0u8; capacity];

    // SAFETY: the buffer is valid for `capacity` bytes.
    let written = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDLISTFDS,
            0,
            buffer.as_mut_ptr() as *mut libc::c_void,
            capacity as libc::c_int,
        )
    };
    if written < 0 {
        return Err(SysError::last_os_error("proc_pidinfo(PROC_PIDLISTFDS)"));
    }

    let written = written as usize;
    let mut ports = Vec::new();
    let mut offset = 0usize;
    while offset + entry_size <= written {
        // SAFETY: the slice holds at least `entry_size` bytes at this offset
        // and `ProcFdInfo` has no invalid bit patterns.
        let fd_info =
            unsafe { ptr::read_unaligned(buffer.as_ptr().add(offset) as *const ProcFdInfo) };
        offset += entry_size;

        if fd_info.proc_fdtype != PROX_FDTYPE_SOCKET {
            continue;
        }

        let mut socket_info = SocketFdInfo::default();
        // SAFETY: `socket_info` is correctly sized and writable.
        let result = unsafe {
            proc_pidfdinfo(
                pid,
                fd_info.proc_fd,
                PROC_PIDFDSOCKETINFO,
                &mut socket_info as *mut _ as *mut libc::c_void,
                size_of::<SocketFdInfo>() as libc::c_int,
            )
        };
        // Descriptors can close between the list and the inspect call; a
        // failure for one fd is not a failure for the process.
        if result <= 0 {
            continue;
        }

        if let Some(listen) = decode_socket_info(&socket_info) {
            if !ports.contains(&listen.port) {
                ports.push(listen.port);
            }
        }
    }

    ports.sort_unstable();
    Ok(ports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::slice;

    const TCP: i32 = SOCKINFO_TCP;
    const UNIX: i32 = 3;

    fn socket_info(kind: i32, state: i32, port: u16, vflag: u8, address: [u8; 16]) -> SocketFdInfo {
        let mut info = SocketFdInfo::default();
        info.psi.soi_kind = kind;
        info.psi.soi_proto.pri_tcp.tcpsi_state = state;
        info.psi.soi_proto.pri_tcp.tcpsi_ini.insi_vflag = vflag;
        info.psi.soi_proto.pri_tcp.tcpsi_ini.insi_laddr = address;
        // insi_lport is an int holding the port in network byte order.
        info.psi.soi_proto.pri_tcp.tcpsi_ini.insi_lport = port.to_be() as i32;
        info
    }

    fn ipv4_loopback() -> [u8; 16] {
        let mut address = [0u8; 16];
        address[12..16].copy_from_slice(&[127, 0, 0, 1]);
        address
    }

    fn ipv6_loopback() -> [u8; 16] {
        let mut address = [0u8; 16];
        address[15] = 1;
        address
    }

    #[test]
    fn field_layout_matches_the_sdk() {
        assert_eq!(size_of::<VinfoStat>(), 136);
        assert_eq!(size_of::<InSockInfo>(), 80);
        assert_eq!(size_of::<TcpSockInfo>(), 120);
        assert_eq!(size_of::<SocketInfo>(), 360);
        assert_eq!(size_of::<SocketFdInfo>(), 384);
    }

    #[test]
    fn extracts_port_from_network_byte_order() {
        let info = socket_info(TCP, TSI_S_LISTEN, 8008, INI_IPV4, ipv4_loopback());
        assert_eq!(decode_socket_info(&info), Some(ListenAddr { port: 8008 }));
    }

    #[test]
    fn accepts_wildcard_and_ipv6_loopback() {
        let wildcard = socket_info(TCP, TSI_S_LISTEN, 8000, INI_IPV4, [0u8; 16]);
        assert_eq!(
            decode_socket_info(&wildcard),
            Some(ListenAddr { port: 8000 })
        );

        let ipv6 = socket_info(TCP, TSI_S_LISTEN, 8008, INI_IPV6, ipv6_loopback());
        assert_eq!(decode_socket_info(&ipv6), Some(ListenAddr { port: 8008 }));
    }

    #[test]
    fn rejects_non_listening_tcp() {
        for state in [0, 2, 3, 4, 5, 10] {
            let info = socket_info(TCP, state, 8008, INI_IPV4, ipv4_loopback());
            assert_eq!(decode_socket_info(&info), None, "state {state}");
        }
    }

    #[test]
    fn rejects_non_tcp_kinds() {
        for kind in [0, 1, UNIX, 4, 5, 6, 7] {
            let info = socket_info(kind, TSI_S_LISTEN, 8008, INI_IPV4, ipv4_loopback());
            assert_eq!(decode_socket_info(&info), None, "kind {kind}");
        }
    }

    #[test]
    fn rejects_non_loopback_addresses() {
        let mut external = [0u8; 16];
        external[12..16].copy_from_slice(&[192, 168, 1, 12]);
        let info = socket_info(TCP, TSI_S_LISTEN, 8008, INI_IPV4, external);
        assert_eq!(decode_socket_info(&info), None);

        let mut external_v6 = [0u8; 16];
        external_v6[0] = 0xfe;
        external_v6[1] = 0x80;
        external_v6[15] = 1;
        let info = socket_info(TCP, TSI_S_LISTEN, 8008, INI_IPV6, external_v6);
        assert_eq!(decode_socket_info(&info), None);
    }

    #[test]
    fn rejects_zero_port() {
        let info = socket_info(TCP, TSI_S_LISTEN, 0, INI_IPV4, ipv4_loopback());
        assert_eq!(decode_socket_info(&info), None);
    }

    #[test]
    fn decodes_a_synthetic_byte_buffer() {
        let info = socket_info(TCP, TSI_S_LISTEN, 8008, INI_IPV4, ipv4_loopback());
        // SAFETY: `SocketFdInfo` is a POD struct; the slice covers it exactly.
        let bytes = unsafe {
            slice::from_raw_parts(
                &info as *const SocketFdInfo as *const u8,
                size_of::<SocketFdInfo>(),
            )
        };
        let copy = bytes.to_vec();
        // SAFETY: `copy` is a byte copy of a valid `SocketFdInfo` and
        // `read_unaligned` tolerates any alignment.
        let restored = unsafe { ptr::read_unaligned(copy.as_ptr() as *const SocketFdInfo) };
        assert_eq!(
            decode_socket_info(&restored),
            Some(ListenAddr { port: 8008 })
        );
    }
}
