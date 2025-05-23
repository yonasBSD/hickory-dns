// Copyright 2015-2023 Benjamin Fry <benjaminfry@me.com>
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// https://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// https://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! SVCB records, see [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460)
#![allow(clippy::use_self)]

use alloc::{string::String, vec::Vec};
use core::{
    cmp::{Ord, Ordering, PartialOrd},
    convert::TryFrom,
    fmt,
};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use enum_as_inner::EnumAsInner;

use crate::{
    error::{ProtoError, ProtoErrorKind, ProtoResult},
    rr::{
        Name, RData, RecordData, RecordDataDecodable, RecordType,
        rdata::{A, AAAA},
    },
    serialize::binary::{
        BinDecodable, BinDecoder, BinEncodable, BinEncoder, RDataEncoding, Restrict, RestrictedMath,
    },
};

///  [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-2.2)
///
/// ```text
/// 2.2.  RDATA wire format
///
///   The RDATA for the SVCB RR consists of:
///
///   *  a 2 octet field for SvcPriority as an integer in network byte
///      order.
///   *  the uncompressed, fully-qualified TargetName, represented as a
///      sequence of length-prefixed labels as in Section 3.1 of [RFC1035].
///   *  the SvcParams, consuming the remainder of the record (so smaller
///      than 65535 octets and constrained by the RDATA and DNS message
///      sizes).
///
///   When the list of SvcParams is non-empty (ServiceMode), it contains a
///   series of SvcParamKey=SvcParamValue pairs, represented as:
///
///   *  a 2 octet field containing the SvcParamKey as an integer in
///      network byte order.  (See Section 14.3.2 for the defined values.)
///   *  a 2 octet field containing the length of the SvcParamValue as an
///      integer between 0 and 65535 in network byte order
///   *  an octet string of this length whose contents are the SvcParamValue
///      in a format determined by the SvcParamKey
///
///   SvcParamKeys SHALL appear in increasing numeric order.
///
///   Clients MUST consider an RR malformed if:
///
///   *  the end of the RDATA occurs within a SvcParam.
///   *  SvcParamKeys are not in strictly increasing numeric order.
///   *  the SvcParamValue for an SvcParamKey does not have the expected
///      format.
///
///   Note that the second condition implies that there are no duplicate
///   SvcParamKeys.
///
///   If any RRs are malformed, the client MUST reject the entire RRSet and
///   fall back to non-SVCB connection establishment.
/// ```
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct SVCB {
    svc_priority: u16,
    target_name: Name,
    svc_params: Vec<(SvcParamKey, SvcParamValue)>,
}

impl SVCB {
    /// Create a new SVCB record from parts
    ///
    /// It is up to the caller to validate the data going into the record
    pub fn new(
        svc_priority: u16,
        target_name: Name,
        svc_params: Vec<(SvcParamKey, SvcParamValue)>,
    ) -> Self {
        Self {
            svc_priority,
            target_name,
            svc_params,
        }
    }

    ///  [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-2.4.1)
    /// ```text
    /// 2.4.1.  SvcPriority
    ///
    ///   When SvcPriority is 0 the SVCB record is in AliasMode
    ///   (Section 2.4.2).  Otherwise, it is in ServiceMode (Section 2.4.3).
    ///
    ///   Within a SVCB RRSet, all RRs SHOULD have the same Mode.  If an RRSet
    ///   contains a record in AliasMode, the recipient MUST ignore any
    ///   ServiceMode records in the set.
    ///
    ///   RRSets are explicitly unordered collections, so the SvcPriority field
    ///   is used to impose an ordering on SVCB RRs.  A smaller SvcPriority indicates
    ///   that the domain owner recommends the use of this record over ServiceMode
    ///   RRs with a larger SvcPriority value.
    ///
    ///   When receiving an RRSet containing multiple SVCB records with the
    ///   same SvcPriority value, clients SHOULD apply a random shuffle within
    ///   a priority level to the records before using them, to ensure uniform
    ///   load-balancing.
    /// ```
    pub fn svc_priority(&self) -> u16 {
        self.svc_priority
    }

    ///  [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-2.5)
    /// ```text
    /// 2.5.  Special handling of "." in TargetName
    ///
    ///   If TargetName has the value "." (represented in the wire format as a
    ///    zero-length label), special rules apply.
    ///
    /// 2.5.1.  AliasMode
    ///
    ///    For AliasMode SVCB RRs, a TargetName of "." indicates that the
    ///    service is not available or does not exist.  This indication is
    ///    advisory: clients encountering this indication MAY ignore it and
    ///    attempt to connect without the use of SVCB.
    ///
    /// 2.5.2.  ServiceMode
    ///
    ///    For ServiceMode SVCB RRs, if TargetName has the value ".", then the
    ///    owner name of this record MUST be used as the effective TargetName.
    ///    If the record has a wildcard owner name in the zone file, the recipient
    ///    SHALL use the response's synthesized owner name as the effective TargetName.
    ///
    ///    Here, for example, "svc2.example.net" is the effective TargetName:
    ///
    ///    example.com.      7200  IN HTTPS 0 svc.example.net.
    ///    svc.example.net.  7200  IN CNAME svc2.example.net.
    ///    svc2.example.net. 7200  IN HTTPS 1 . port=8002
    ///    svc2.example.net. 300   IN A     192.0.2.2
    ///    svc2.example.net. 300   IN AAAA  2001:db8::2
    /// ```
    pub fn target_name(&self) -> &Name {
        &self.target_name
    }

    /// See [`SvcParamKey`] for details on each parameter
    pub fn svc_params(&self) -> &[(SvcParamKey, SvcParamValue)] {
        &self.svc_params
    }
}

///  [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-14.3.2)
///
/// ```text
/// 14.3.2.  Initial Contents
///
///    The "Service Parameter Keys (SvcParamKeys)" registry has been
///    populated with the following initial registrations:
///
///    +===========+=================+================+=========+==========+
///    |   Number  | Name            | Meaning        |Reference|Change    |
///    |           |                 |                |         |Controller|
///    +===========+=================+================+=========+==========+
///    |     0     | mandatory       | Mandatory      |RFC 9460,|IETF      |
///    |           |                 | keys in this   |Section 8|          |
///    |           |                 | RR             |         |          |
///    +-----------+-----------------+----------------+---------+----------+
///    |     1     | alpn            | Additional     |RFC 9460,|IETF      |
///    |           |                 | supported      |Section  |          |
///    |           |                 | protocols      |7.1      |          |
///    +-----------+-----------------+----------------+---------+----------+
///    |     2     | no-default-alpn | No support     |RFC 9460,|IETF      |
///    |           |                 | for default    |Section  |          |
///    |           |                 | protocol       |7.1      |          |
///    +-----------+-----------------+----------------+---------+----------+
///    |     3     | port            | Port for       |RFC 9460,|IETF      |
///    |           |                 | alternative    |Section  |          |
///    |           |                 | endpoint       |7.2      |          |
///    +-----------+-----------------+----------------+---------+----------+
///    |     4     | ipv4hint        | IPv4 address   |RFC 9460,|IETF      |
///    |           |                 | hints          |Section  |          |
///    |           |                 |                |7.3      |          |
///    +-----------+-----------------+----------------+---------+----------+
///    |     5     | ech             | RESERVED       |N/A      |IETF      |
///    |           |                 | (held for      |         |          |
///    |           |                 | Encrypted      |         |          |
///    |           |                 | ClientHello)   |         |          |
///    +-----------+-----------------+----------------+---------+----------+
///    |     6     | ipv6hint        | IPv6 address   |RFC 9460,|IETF      |
///    |           |                 | hints          |Section  |          |
///    |           |                 |                |7.3      |          |
///    +-----------+-----------------+----------------+---------+----------+
///    |65280-65534| N/A             | Reserved for   |RFC 9460 |IETF      |
///    |           |                 | Private Use    |         |          |
///    +-----------+-----------------+----------------+---------+----------+
///    |   65535   | N/A             | Reserved       |RFC 9460 |IETF      |
///    |           |                 | ("Invalid      |         |          |
///    |           |                 | key")          |         |          |
///    +-----------+-----------------+----------------+---------+----------+
///
/// parsing done via:
///   *  a 2 octet field containing the SvcParamKey as an integer in
///      network byte order.  (See Section 14.3.2 for the defined values.)
/// ```
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum SvcParamKey {
    /// Mandatory keys in this RR
    #[cfg_attr(feature = "serde", serde(rename = "mandatory"))]
    Mandatory,
    /// Additional supported protocols
    #[cfg_attr(feature = "serde", serde(rename = "alpn"))]
    Alpn,
    /// No support for default protocol
    #[cfg_attr(feature = "serde", serde(rename = "no-default-alpn"))]
    NoDefaultAlpn,
    /// Port for alternative endpoint
    #[cfg_attr(feature = "serde", serde(rename = "port"))]
    Port,
    /// IPv4 address hints
    #[cfg_attr(feature = "serde", serde(rename = "ipv4hint"))]
    Ipv4Hint,
    /// Encrypted Client Hello configuration list
    #[cfg_attr(feature = "serde", serde(rename = "ech"))]
    EchConfigList,
    /// IPv6 address hints
    #[cfg_attr(feature = "serde", serde(rename = "ipv6hint"))]
    Ipv6Hint,
    /// Private Use
    Key(u16),
    /// Reserved ("Invalid key")
    Key65535,
    /// Unknown
    Unknown(u16),
}

impl From<u16> for SvcParamKey {
    fn from(val: u16) -> Self {
        match val {
            0 => Self::Mandatory,
            1 => Self::Alpn,
            2 => Self::NoDefaultAlpn,
            3 => Self::Port,
            4 => Self::Ipv4Hint,
            5 => Self::EchConfigList,
            6 => Self::Ipv6Hint,
            65280..=65534 => Self::Key(val),
            65535 => Self::Key65535,
            _ => Self::Unknown(val),
        }
    }
}

impl From<SvcParamKey> for u16 {
    fn from(val: SvcParamKey) -> Self {
        match val {
            SvcParamKey::Mandatory => 0,
            SvcParamKey::Alpn => 1,
            SvcParamKey::NoDefaultAlpn => 2,
            SvcParamKey::Port => 3,
            SvcParamKey::Ipv4Hint => 4,
            SvcParamKey::EchConfigList => 5,
            SvcParamKey::Ipv6Hint => 6,
            SvcParamKey::Key(val) => val,
            SvcParamKey::Key65535 => 65535,
            SvcParamKey::Unknown(val) => val,
        }
    }
}

impl<'r> BinDecodable<'r> for SvcParamKey {
    // a 2 octet field containing the SvcParamKey as an integer in
    //      network byte order.  (See Section 14.3.2 for the defined values.)
    fn read(decoder: &mut BinDecoder<'r>) -> ProtoResult<Self> {
        Ok(decoder.read_u16()?.unverified(/*any u16 is valid*/).into())
    }
}

impl BinEncodable for SvcParamKey {
    // a 2 octet field containing the SvcParamKey as an integer in
    //      network byte order.  (See Section 14.3.2 for the defined values.)
    fn emit(&self, encoder: &mut BinEncoder<'_>) -> ProtoResult<()> {
        encoder.emit_u16((*self).into())
    }
}

impl fmt::Display for SvcParamKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        match self {
            Self::Mandatory => f.write_str("mandatory")?,
            Self::Alpn => f.write_str("alpn")?,
            Self::NoDefaultAlpn => f.write_str("no-default-alpn")?,
            Self::Port => f.write_str("port")?,
            Self::Ipv4Hint => f.write_str("ipv4hint")?,
            Self::EchConfigList => f.write_str("ech")?,
            Self::Ipv6Hint => f.write_str("ipv6hint")?,
            Self::Key(val) => write!(f, "key{val}")?,
            Self::Key65535 => f.write_str("key65535")?,
            Self::Unknown(val) => write!(f, "unknown{val}")?,
        }

        Ok(())
    }
}

impl core::str::FromStr for SvcParamKey {
    type Err = ProtoError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        /// keys are in the format of key#, e.g. key12344, with a max value of u16
        fn parse_unknown_key(key: &str) -> Result<SvcParamKey, ProtoError> {
            let key_value = key.strip_prefix("key").ok_or_else(|| {
                ProtoError::from(ProtoErrorKind::Msg(format!(
                    "bad formatted key ({key}), expected key1234"
                )))
            })?;

            Ok(SvcParamKey::Key(u16::from_str(key_value)?))
        }

        let key = match s {
            "mandatory" => Self::Mandatory,
            "alpn" => Self::Alpn,
            "no-default-alpn" => Self::NoDefaultAlpn,
            "port" => Self::Port,
            "ipv4hint" => Self::Ipv4Hint,
            "ech" => Self::EchConfigList,
            "ipv6hint" => Self::Ipv6Hint,
            "key65535" => Self::Key65535,
            _ => parse_unknown_key(s)?,
        };

        Ok(key)
    }
}

impl Ord for SvcParamKey {
    fn cmp(&self, other: &Self) -> Ordering {
        u16::from(*self).cmp(&u16::from(*other))
    }
}

impl PartialOrd for SvcParamKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Warning, it is currently up to users of this type to validate the data against that expected by the key
///
/// ```text
///   *  a 2 octet field containing the length of the SvcParamValue as an
///      integer between 0 and 65535 in network byte order (but constrained
///      by the RDATA and DNS message sizes).
///   *  an octet string of this length whose contents are in a format
///      determined by the SvcParamKey.
/// ```
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, PartialEq, Eq, Hash, Clone, EnumAsInner)]
pub enum SvcParamValue {
    ///    In a ServiceMode RR, a SvcParamKey is considered "mandatory" if the
    ///    RR will not function correctly for clients that ignore this
    ///    SvcParamKey.  Each SVCB protocol mapping SHOULD specify a set of keys
    ///    that are "automatically mandatory", i.e. mandatory if they are
    ///    present in an RR.  The SvcParamKey "mandatory" is used to indicate
    ///    any mandatory keys for this RR, in addition to any automatically
    ///    mandatory keys that are present.
    ///
    /// see `Mandatory`
    #[cfg_attr(feature = "serde", serde(rename = "mandatory"))]
    Mandatory(Mandatory),
    ///  [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-7.1)
    ///
    /// ```text
    ///    The "alpn" and "no-default-alpn" SvcParamKeys together indicate the
    ///    set of Application Layer Protocol Negotiation (ALPN) protocol
    ///    identifiers [Alpn] and associated transport protocols supported by
    ///    this service endpoint (the "SVCB ALPN set").
    /// ```
    #[cfg_attr(feature = "serde", serde(rename = "alpn"))]
    Alpn(Alpn),
    /// For "no-default-alpn", the presentation and wire format values MUST
    ///    be empty.
    /// See also `Alpn`
    #[cfg_attr(feature = "serde", serde(rename = "no-default-alpn"))]
    NoDefaultAlpn,
    ///  [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-7.2)
    ///
    /// ```text
    ///    7.2.  "port"
    ///
    ///   The "port" SvcParamKey defines the TCP or UDP port that should be
    ///   used to reach this alternative endpoint.  If this key is not present,
    ///   clients SHALL use the authority endpoint's port number.
    ///
    ///   The presentation value of the SvcParamValue is a single decimal
    ///   integer between 0 and 65535 in ASCII.  Any other value (e.g. an
    ///   empty value) is a syntax error.  To enable simpler parsing, this
    ///   SvcParam MUST NOT contain escape sequences.
    ///
    ///   The wire format of the SvcParamValue is the corresponding 2 octet
    ///   numeric value in network byte order.
    ///
    ///   If a port-restricting firewall is in place between some client and
    ///   the service endpoint, changing the port number might cause that
    ///   client to lose access to the service, so operators should exercise
    ///   caution when using this SvcParamKey to specify a non-default port.
    /// ```
    #[cfg_attr(feature = "serde", serde(rename = "port"))]
    Port(u16),
    ///  [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-7.2)
    ///
    ///   The "ipv4hint" and "ipv6hint" keys convey IP addresses that clients
    ///   MAY use to reach the service.  If A and AAAA records for TargetName
    ///   are locally available, the client SHOULD ignore these hints.
    ///   Otherwise, clients SHOULD perform A and/or AAAA queries for
    ///   TargetName as in Section 3, and clients SHOULD use the IP address in
    ///   those responses for future connections.  Clients MAY opt to terminate
    ///   any connections using the addresses in hints and instead switch to
    ///   the addresses in response to the TargetName query.  Failure to use A
    ///   and/or AAAA response addresses could negatively impact load balancing
    ///   or other geo-aware features and thereby degrade client performance.
    ///
    /// see `IpHint`
    #[cfg_attr(feature = "serde", serde(rename = "ipv4hint"))]
    Ipv4Hint(IpHint<A>),
    /// [draft-ietf-tls-svcb-ech-01 Bootstrapping TLS Encrypted ClientHello with DNS Service Bindings, Sep 2024](https://datatracker.ietf.org/doc/html/draft-ietf-tls-svcb-ech-01)
    ///
    /// ```text
    /// 2.  "SvcParam for ECH configuration"
    ///
    ///   The "ech" SvcParamKey is defined for conveying the ECH configuration
    ///   of an alternative endpoint. It is applicable to all TLS-based protocols
    ///   (including DTLS [RFC9147] and QUIC version 1 [RFC9001]) unless otherwise
    ///   specified.
    /// ```
    #[cfg_attr(feature = "serde", serde(rename = "ech"))]
    EchConfigList(EchConfigList),
    /// See `IpHint`
    #[cfg_attr(feature = "serde", serde(rename = "ipv6hint"))]
    Ipv6Hint(IpHint<AAAA>),
    /// Unparsed network data. Refer to documents on the associated key value
    ///
    /// This will be left as is when read off the wire, and encoded in bas64
    ///    for presentation.
    Unknown(Unknown),
}

impl SvcParamValue {
    // a 2 octet field containing the length of the SvcParamValue as an
    //      integer between 0 and 65535 in network byte order (but constrained
    //      by the RDATA and DNS message sizes).
    fn read(key: SvcParamKey, decoder: &mut BinDecoder<'_>) -> ProtoResult<Self> {
        let len: usize = decoder
            .read_u16()?
            .verify_unwrap(|len| *len as usize <= decoder.len())
            .map(|len| len as usize)
            .map_err(|u| {
                ProtoError::from(format!(
                    "length of SvcParamValue ({}) exceeds remainder in RDATA ({})",
                    u,
                    decoder.len()
                ))
            })?;

        let param_data = decoder.read_slice(len)?.unverified(/*verification to be done by individual param types*/);
        let mut decoder = BinDecoder::new(param_data);

        let value = match key {
            SvcParamKey::Mandatory => Self::Mandatory(Mandatory::read(&mut decoder)?),
            SvcParamKey::Alpn => Self::Alpn(Alpn::read(&mut decoder)?),
            // should always be empty
            SvcParamKey::NoDefaultAlpn => {
                if len > 0 {
                    return Err(ProtoError::from("Alpn expects at least one value"));
                }

                Self::NoDefaultAlpn
            }
            // The wire format of the SvcParamValue is the corresponding 2 octet
            // numeric value in network byte order.
            SvcParamKey::Port => {
                let port = decoder.read_u16()?.unverified(/*all values are legal ports*/);
                Self::Port(port)
            }
            SvcParamKey::Ipv4Hint => Self::Ipv4Hint(IpHint::<A>::read(&mut decoder)?),
            SvcParamKey::EchConfigList => Self::EchConfigList(EchConfigList::read(&mut decoder)?),
            SvcParamKey::Ipv6Hint => Self::Ipv6Hint(IpHint::<AAAA>::read(&mut decoder)?),
            SvcParamKey::Key(_) | SvcParamKey::Key65535 | SvcParamKey::Unknown(_) => {
                Self::Unknown(Unknown::read(&mut decoder)?)
            }
        };

        Ok(value)
    }
}

impl BinEncodable for SvcParamValue {
    // a 2 octet field containing the length of the SvcParamValue as an
    //      integer between 0 and 65535 in network byte order (but constrained
    //      by the RDATA and DNS message sizes).
    fn emit(&self, encoder: &mut BinEncoder<'_>) -> ProtoResult<()> {
        // set the place for the length...
        let place = encoder.place::<u16>()?;

        match self {
            Self::Mandatory(mandatory) => mandatory.emit(encoder)?,
            Self::Alpn(alpn) => alpn.emit(encoder)?,
            Self::NoDefaultAlpn => (),
            Self::Port(port) => encoder.emit_u16(*port)?,
            Self::Ipv4Hint(ip_hint) => ip_hint.emit(encoder)?,
            Self::EchConfigList(ech_config) => ech_config.emit(encoder)?,
            Self::Ipv6Hint(ip_hint) => ip_hint.emit(encoder)?,
            Self::Unknown(unknown) => unknown.emit(encoder)?,
        }

        // go back and set the length
        let len = u16::try_from(encoder.len_since_place(&place))
            .map_err(|_| ProtoError::from("Total length of SvcParamValue exceeds u16::MAX"))?;
        place.replace(encoder, len)?;

        Ok(())
    }
}

impl fmt::Display for SvcParamValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        match self {
            Self::Mandatory(mandatory) => write!(f, "{mandatory}")?,
            Self::Alpn(alpn) => write!(f, "{alpn}")?,
            Self::NoDefaultAlpn => (),
            Self::Port(port) => write!(f, "{port}")?,
            Self::Ipv4Hint(ip_hint) => write!(f, "{ip_hint}")?,
            Self::EchConfigList(ech_config) => write!(f, "{ech_config}")?,
            Self::Ipv6Hint(ip_hint) => write!(f, "{ip_hint}")?,
            Self::Unknown(unknown) => write!(f, "{unknown}")?,
        }

        Ok(())
    }
}

///  [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-8)
///
/// ```text
/// 8.  ServiceMode RR compatibility and mandatory keys
///
///    In a ServiceMode RR, a SvcParamKey is considered "mandatory" if the
///    RR will not function correctly for clients that ignore this
///    SvcParamKey.  Each SVCB protocol mapping SHOULD specify a set of keys
///    that are "automatically mandatory", i.e. mandatory if they are
///    present in an RR.  The SvcParamKey "mandatory" is used to indicate
///    any mandatory keys for this RR, in addition to any automatically
///    mandatory keys that are present.
///
///    A ServiceMode RR is considered "compatible" with a client if the
///    client recognizes all the mandatory keys, and their values indicate
///    that successful connection establishment is possible. Incompatible RRs
///    are ignored (see step 5 of the procedure defined in Section 3)
///
///    The presentation value SHALL be a comma-separated list
///    (Appendix A.1) of one or more valid SvcParamKeys, either by their
///    registered name or in the unknown-key format (Section 2.1).  Keys MAY
///    appear in any order, but MUST NOT appear more than once.  For self-
///    consistency (Section 2.4.3), listed keys MUST also appear in the
///    SvcParams.
///
///    To enable simpler parsing, this SvcParamValue MUST NOT contain escape
///    sequences.
///
///    For example, the following is a valid list of SvcParams:
///
///    ipv6hint=... key65333=ex1 key65444=ex2 mandatory=key65444,ipv6hint
///
///    In wire format, the keys are represented by their numeric values in
///    network byte order, concatenated in strictly increasing numeric order.
///
///    This SvcParamKey is always automatically mandatory, and MUST NOT
///    appear in its own value-list.  Other automatically mandatory keys
///    SHOULD NOT appear in the list either.  (Including them wastes space
///    and otherwise has no effect.)
/// ```
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
#[repr(transparent)]
pub struct Mandatory(pub Vec<SvcParamKey>);

impl<'r> BinDecodable<'r> for Mandatory {
    /// This expects the decoder to be limited to only this field, i.e. the end of input for the decoder
    ///   is the end of input for the fields
    ///
    /// ```text
    ///    In wire format, the keys are represented by their numeric values in
    ///    network byte order, concatenated in strictly increasing numeric order.
    /// ```
    fn read(decoder: &mut BinDecoder<'r>) -> ProtoResult<Self> {
        let mut keys = Vec::with_capacity(1);

        while decoder.peek().is_some() {
            keys.push(SvcParamKey::read(decoder)?);
        }

        if keys.is_empty() {
            return Err(ProtoError::from("Mandatory expects at least one value"));
        }

        Ok(Self(keys))
    }
}

impl BinEncodable for Mandatory {
    /// This expects the decoder to be limited to only this field, i.e. the end of input for the decoder
    ///   is the end of input for the fields
    ///
    /// ```text
    ///    In wire format, the keys are represented by their numeric values in
    ///    network byte order, concatenated in strictly increasing numeric order.
    /// ```
    fn emit(&self, encoder: &mut BinEncoder<'_>) -> ProtoResult<()> {
        if self.0.is_empty() {
            return Err(ProtoError::from("Alpn expects at least one value"));
        }

        // TODO: order by key value
        for key in self.0.iter() {
            key.emit(encoder)?
        }

        Ok(())
    }
}

impl fmt::Display for Mandatory {
    ///  [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-8)
    ///
    ///    The presentation value SHALL be a comma-separated list
    ///    (Appendix A.1) of one or more valid SvcParamKeys, either by their
    ///    registered name or in the unknown-key format (Section 2.1).  Keys MAY
    ///    appear in any order, but MUST NOT appear more than once.  For self-
    ///    consistency (Section 2.4.3), listed keys MUST also appear in the
    ///    SvcParams.
    ///
    ///    To enable simpler parsing, this SvcParamValue MUST NOT contain escape
    ///    sequences.
    ///
    ///    For example, the following is a valid list of SvcParams:
    ///
    ///    ipv6hint=... key65333=ex1 key65444=ex2 mandatory=key65444,ipv6hint
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        for key in self.0.iter() {
            // TODO: confirm in the RFC that trailing commas are ok
            write!(f, "{key},")?;
        }

        Ok(())
    }
}

///  [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-7.1)
///
/// ```text
/// 7.1.  "alpn" and "no-default-alpn"
///
///   The "alpn" and "no-default-alpn" SvcParamKeys together indicate the
///   set of Application-Layer Protocol Negotiation (ALPN) protocol
///   identifiers [ALPN] and associated transport protocols supported by
///   this service endpoint (the "SVCB ALPN set").
///
///   As with Alt-Svc [AltSvc], each ALPN protocol identifier is used to
///   identify the application protocol and associated suite of protocols
///   supported by the endpoint (the "protocol suite").  The presence of an
///   ALPN protocol identifier in the SVCB ALPN set indicates that this
///   service endpoint, described by TargetName and the other parameters
///   (e.g., "port"), offers service with the protocol suite associated
///   with this ALPN identifier.
///
///   Clients filter the set of ALPN identifiers to match the protocol
///   suites they support, and this informs the underlying transport
///   protocol used (such as QUIC over UDP or TLS over TCP).  ALPN protocol
///   identifiers that do not uniquely identify a protocol suite (e.g., an
///   Identification Sequence that can be used with both TLS and DTLS) are
///   not compatible with this SvcParamKey and MUST NOT be included in the
///   SVCB ALPN set.
///
/// 7.1.1.  Representation
///
///   ALPNs are identified by their registered "Identification Sequence"
///   (alpn-id), which is a sequence of 1-255 octets.
///
///   alpn-id = 1*255OCTET
///
///   For "alpn", the presentation value SHALL be a comma-separated list
///   (Appendix A.1) of one or more alpn-ids.  Zone-file implementations
///   MAY disallow the "," and "\" characters in ALPN IDs instead of
///   implementing the value-list escaping procedure, relying on the opaque
///   key format (e.g., key1=\002h2) in the event that these characters are
///   needed.
///
///   The wire-format value for "alpn" consists of at least one alpn-id
///   prefixed by its length as a single octet, and these length-value
///   pairs are concatenated to form the SvcParamValue.  These pairs MUST
///   exactly fill the SvcParamValue; otherwise, the SvcParamValue is
///   malformed.
///
///   For "no-default-alpn", the presentation and wire-format values MUST
///   be empty.  When "no-default-alpn" is specified in an RR, "alpn" must
///   also be specified in order for the RR to be "self-consistent"
///   (Section 2.4.3).
///
///   Each scheme that uses this SvcParamKey defines a "default set" of
///   ALPN IDs that are supported by nearly all clients and servers; this
///   set MAY be empty.  To determine the SVCB ALPN set, the client starts
///   with the list of alpn-ids from the "alpn" SvcParamKey, and it adds
///   the default set unless the "no-default-alpn" SvcParamKey is present.
///
/// 7.1.2.  Use
///
///   To establish a connection to the endpoint, clients MUST
///
///   1.  Let SVCB-ALPN-Intersection be the set of protocols in the SVCB
///       ALPN set that the client supports.
///
///   2.  Let Intersection-Transports be the set of transports (e.g., TLS,
///       DTLS, QUIC) implied by the protocols in SVCB-ALPN-Intersection.
///
///   3.  For each transport in Intersection-Transports, construct a
///       ProtocolNameList containing the Identification Sequences of all
///       the client's supported ALPN protocols for that transport, without
///       regard to the SVCB ALPN set.
///
///   For example, if the SVCB ALPN set is ["http/1.1", "h3"] and the
///   client supports HTTP/1.1, HTTP/2, and HTTP/3, the client could
///   attempt to connect using TLS over TCP with a ProtocolNameList of
///   ["http/1.1", "h2"] and could also attempt a connection using QUIC
///   with a ProtocolNameList of ["h3"].
///
///   Once the client has constructed a ClientHello, protocol negotiation
///   in that handshake proceeds as specified in [ALPN], without regard to
///   the SVCB ALPN set.
///
///   Clients MAY implement a fallback procedure, using a less-preferred
///   transport if more-preferred transports fail to connect.  This
///   fallback behavior is vulnerable to manipulation by a network attacker
///   who blocks the more-preferred transports, but it may be necessary for
///   compatibility with existing networks.
///
///   With this procedure in place, an attacker who can modify DNS and
///   network traffic can prevent a successful transport connection but
///   cannot otherwise interfere with ALPN protocol selection.  This
///   procedure also ensures that each ProtocolNameList includes at least
///   one protocol from the SVCB ALPN set.
///
///   Clients SHOULD NOT attempt connection to a service endpoint whose
///   SVCB ALPN set does not contain any supported protocols.
///
///   To ensure consistency of behavior, clients MAY reject the entire SVCB
///   RRset and fall back to basic connection establishment if all of the
///   compatible RRs indicate "no-default-alpn", even if connection could
///   have succeeded using a non-default ALPN protocol.
///
///   Zone operators SHOULD ensure that at least one RR in each RRset
///   supports the default transports.  This enables compatibility with the
///   greatest number of clients.
/// ```
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
#[repr(transparent)]
pub struct Alpn(pub Vec<String>);

impl<'r> BinDecodable<'r> for Alpn {
    /// This expects the decoder to be limited to only this field, i.e. the end of input for the decoder
    ///   is the end of input for the fields
    ///
    /// ```text
    ///   The wire format value for "alpn" consists of at least one alpn-id
    ///   prefixed by its length as a single octet, and these length-value
    ///   pairs are concatenated to form the SvcParamValue.  These pairs MUST
    ///   exactly fill the SvcParamValue; otherwise, the SvcParamValue is
    ///   malformed.
    /// ```
    fn read(decoder: &mut BinDecoder<'r>) -> ProtoResult<Self> {
        let mut alpns = Vec::with_capacity(1);

        while decoder.peek().is_some() {
            let alpn = decoder.read_character_data()?.unverified(/*will rely on string parser*/);
            let alpn = String::from_utf8(alpn.to_vec())?;
            alpns.push(alpn);
        }

        if alpns.is_empty() {
            return Err(ProtoError::from("Alpn expects at least one value"));
        }

        Ok(Self(alpns))
    }
}

impl BinEncodable for Alpn {
    ///   The wire format value for "alpn" consists of at least one alpn-id
    ///   prefixed by its length as a single octet, and these length-value
    ///   pairs are concatenated to form the SvcParamValue.  These pairs MUST
    ///   exactly fill the SvcParamValue; otherwise, the SvcParamValue is
    ///   malformed.
    fn emit(&self, encoder: &mut BinEncoder<'_>) -> ProtoResult<()> {
        if self.0.is_empty() {
            return Err(ProtoError::from("Alpn expects at least one value"));
        }

        for alpn in self.0.iter() {
            encoder.emit_character_data(alpn)?
        }

        Ok(())
    }
}

impl fmt::Display for Alpn {
    ///   The presentation value SHALL be a comma-separated list
    ///   (Appendix A.1) of one or more "alpn-id"s.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        for alpn in self.0.iter() {
            // TODO: confirm in the RFC that trailing commas are ok
            write!(f, "{alpn},")?;
        }

        Ok(())
    }
}

/// [draft-ietf-tls-svcb-ech-01 Bootstrapping TLS Encrypted ClientHello with DNS Service Bindings, Sep 2024](https://datatracker.ietf.org/doc/html/draft-ietf-tls-svcb-ech-01)
///
/// ```text
/// 2.  "SvcParam for ECH configuration"
///
///   The "ech" SvcParamKey is defined for conveying the ECH configuration
///   of an alternative endpoint. It is applicable to all TLS-based protocols
///   (including DTLS [RFC9147] and QUIC version 1 [RFC9001]) unless
///   otherwise specified.
///
///   In wire format, the value of the parameter is an ECHConfigList (Section 4 of draft-ietf-tls-esni-18),
///   including the redundant length prefix. In presentation format, the value is the ECHConfigList
///   in Base 64 Encoding (Section 4 of [RFC4648]). Base 64 is used here to simplify integration
///   with TLS server software. To enable simpler parsing, this SvcParam MUST NOT contain escape
///   sequences.
/// ```
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(PartialEq, Eq, Hash, Clone)]
#[repr(transparent)]
pub struct EchConfigList(pub Vec<u8>);

impl<'r> BinDecodable<'r> for EchConfigList {
    /// In wire format, the value of the parameter is an ECHConfigList (Section 4 of draft-ietf-tls-esni-18),
    /// including the redundant length prefix. In presentation format, the value is the
    /// ECHConfigList in Base 64 Encoding (Section 4 of RFC4648).
    /// Base 64 is used here to simplify integration with TLS server software.
    /// To enable simpler parsing, this SvcParam MUST NOT contain escape sequences.
    fn read(decoder: &mut BinDecoder<'r>) -> ProtoResult<Self> {
        let data =
            decoder.read_vec(decoder.len())?.unverified(/*up to consumer to validate this data*/);

        Ok(Self(data))
    }
}

impl BinEncodable for EchConfigList {
    /// In wire format, the value of the parameter is an ECHConfigList (Section 4 of draft-ietf-tls-esni-18),
    /// including the redundant length prefix. In presentation format, the value is the
    /// ECHConfigList in Base 64 Encoding (Section 4 of RFC4648).
    /// Base 64 is used here to simplify integration with TLS server software.
    /// To enable simpler parsing, this SvcParam MUST NOT contain escape sequences.
    fn emit(&self, encoder: &mut BinEncoder<'_>) -> ProtoResult<()> {
        encoder.emit_vec(&self.0)?;

        Ok(())
    }
}

impl fmt::Display for EchConfigList {
    /// As the documentation states, the presentation format (what this function outputs) must be a BASE64 encoded string.
    ///   hickory-dns will encode to BASE64 during formatting of the internal data, and output the BASE64 value.
    ///
    /// [draft-ietf-tls-svcb-ech-01 Bootstrapping TLS Encrypted ClientHello with DNS Service Bindings, Sep 2024](https://datatracker.ietf.org/doc/html/draft-ietf-tls-svcb-ech-01)
    /// ```text
    ///  In presentation format, the value is the ECHConfigList in Base 64 Encoding
    ///  (Section 4 of [RFC4648]). Base 64 is used here to simplify integration with
    ///  TLS server software. To enable simpler parsing, this SvcParam MUST NOT
    ///  contain escape sequences.
    /// ```
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(f, "\"{}\"", data_encoding::BASE64.encode(&self.0))
    }
}

impl fmt::Debug for EchConfigList {
    /// The debug format for EchConfig will output the value in BASE64 like Display, but will the addition of the type-name.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(
            f,
            "\"EchConfig ({})\"",
            data_encoding::BASE64.encode(&self.0)
        )
    }
}

///  [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-7.3)
///
/// ```text
///    7.3.  "ipv4hint" and "ipv6hint"
///
///   The "ipv4hint" and "ipv6hint" keys convey IP addresses that clients
///   MAY use to reach the service.  If A and AAAA records for TargetName
///   are locally available, the client SHOULD ignore these hints.
///   Otherwise, clients SHOULD perform A and/or AAAA queries for
///   TargetName per Section 3, and clients SHOULD use the IP address in
///   those responses for future connections.  Clients MAY opt to terminate
///   any connections using the addresses in hints and instead switch to
///   the addresses in response to the TargetName query.  Failure to use A
///   and/or AAAA response addresses could negatively impact load balancing
///   or other geo-aware features and thereby degrade client performance.
///
///   The presentation value SHALL be a comma-separated list (Appendix A.1)
///   of one or more IP addresses of the appropriate family in standard
///   textual format [RFC5952] [RFC4001].  To enable simpler parsing, this
///   SvcParamValue MUST NOT contain escape sequences.
///
///   The wire format for each parameter is a sequence of IP addresses in
///   network byte order (for the respective address family).  Like an A or
///   AAAA RRset, the list of addresses represents an unordered collection,
///   and clients SHOULD pick addresses to use in a random order.  An empty
///   list of addresses is invalid.
///
///   When selecting between IPv4 and IPv6 addresses to use, clients may
///   use an approach such as Happy Eyeballs [HappyEyeballsV2].  When only
///   "ipv4hint" is present, NAT64 clients may synthesize IPv6 addresses as
///   specified in [RFC7050] or ignore the "ipv4hint" key and wait for AAAA
///   resolution (Section 3).  For best performance, server operators
///   SHOULD include an "ipv6hint" parameter whenever they include an
///   "ipv4hint" parameter.
///
///   These parameters are intended to minimize additional connection
///   latency when a recursive resolver is not compliant with the
///   requirements in Section 4 and SHOULD NOT be included if most clients
///   are using compliant recursive resolvers.  When TargetName is the
///   service name or the owner name (which can be written as "."), server
///   operators SHOULD NOT include these hints, because they are unlikely
///   to convey any performance benefit.
/// ```
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
#[repr(transparent)]
pub struct IpHint<T>(pub Vec<T>);

impl<'r, T> BinDecodable<'r> for IpHint<T>
where
    T: BinDecodable<'r>,
{
    ///   The wire format for each parameter is a sequence of IP addresses in
    ///   network byte order (for the respective address family). Like an A or
    ///   AAAA RRSet, the list of addresses represents an unordered collection,
    ///   and clients SHOULD pick addresses to use in a random order.  An empty
    ///   list of addresses is invalid.
    fn read(decoder: &mut BinDecoder<'r>) -> ProtoResult<Self> {
        let mut ips = Vec::new();

        while decoder.peek().is_some() {
            ips.push(T::read(decoder)?)
        }

        Ok(Self(ips))
    }
}

impl<T> BinEncodable for IpHint<T>
where
    T: BinEncodable,
{
    ///   The wire format for each parameter is a sequence of IP addresses in
    ///   network byte order (for the respective address family). Like an A or
    ///   AAAA RRSet, the list of addresses represents an unordered collection,
    ///   and clients SHOULD pick addresses to use in a random order.  An empty
    ///   list of addresses is invalid.
    fn emit(&self, encoder: &mut BinEncoder<'_>) -> ProtoResult<()> {
        for ip in self.0.iter() {
            ip.emit(encoder)?;
        }

        Ok(())
    }
}

impl<T> fmt::Display for IpHint<T>
where
    T: fmt::Display,
{
    ///   The presentation value SHALL be a comma-separated list
    ///   (Appendix A.1) of one or more IP addresses of the appropriate family
    ///   in standard textual format [RFC 5952](https://tools.ietf.org/html/rfc5952).  To enable simpler parsing,
    ///   this SvcParamValue MUST NOT contain escape sequences.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        for ip in self.0.iter() {
            write!(f, "{ip},")?;
        }

        Ok(())
    }
}

///  [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-2.1)
///
/// ```text
///   Arbitrary keys can be represented using the unknown-key presentation
///   format "keyNNNNN" where NNNNN is the numeric value of the key type
///   without leading zeros. A SvcParam in this form SHALL be parsed as specified
///   above, and the decoded value SHALL be used as its wire-format encoding.
///
///   For some SvcParamKeys, the value corresponds to a list or set of
///   items.  Presentation formats for such keys SHOULD use a comma-
///   separated list (Appendix A.1).
///
///   SvcParams in presentation format MAY appear in any order, but keys
///   MUST NOT be repeated.
/// ```
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
#[repr(transparent)]
pub struct Unknown(pub Vec<u8>);

impl<'r> BinDecodable<'r> for Unknown {
    fn read(decoder: &mut BinDecoder<'r>) -> ProtoResult<Self> {
        // The passed slice is already length delimited, and we cannot
        // assume it's a collection of anything.
        let len = decoder.len();

        let data = decoder.read_vec(len)?;
        let unknowns = data.unverified(/*any data is valid here*/).to_vec();

        Ok(Self(unknowns))
    }
}

impl BinEncodable for Unknown {
    fn emit(&self, encoder: &mut BinEncoder<'_>) -> ProtoResult<()> {
        encoder.emit_vec(&self.0)?;

        Ok(())
    }
}

impl fmt::Display for Unknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        // TODO: this needs to be properly encoded
        write!(f, "\"{}\",", String::from_utf8_lossy(&self.0))?;

        Ok(())
    }
}

impl BinEncodable for SVCB {
    fn emit(&self, encoder: &mut BinEncoder<'_>) -> ProtoResult<()> {
        let mut encoder = encoder.with_rdata_behavior(RDataEncoding::Other);

        self.svc_priority.emit(&mut encoder)?;
        self.target_name.emit(&mut encoder)?;

        let mut last_key: Option<SvcParamKey> = None;
        for (key, param) in self.svc_params.iter() {
            if let Some(last_key) = last_key {
                if key <= &last_key {
                    return Err(ProtoError::from("SvcParams out of order"));
                }
            }

            key.emit(&mut encoder)?;
            param.emit(&mut encoder)?;

            last_key = Some(*key);
        }

        Ok(())
    }
}

impl RecordDataDecodable<'_> for SVCB {
    /// Reads the SVCB record from the decoder.
    ///
    /// [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-2.2)
    ///
    /// ```text
    ///   Clients MUST consider an RR malformed if:
    ///
    ///   *  the end of the RDATA occurs within a SvcParam.
    ///   *  SvcParamKeys are not in strictly increasing numeric order.
    ///   *  the SvcParamValue for an SvcParamKey does not have the expected
    ///      format.
    ///
    ///   Note that the second condition implies that there are no duplicate
    ///   SvcParamKeys.
    ///
    ///   If any RRs are malformed, the client MUST reject the entire RRSet and
    ///   fall back to non-SVCB connection establishment.
    /// ```
    fn read_data(decoder: &mut BinDecoder<'_>, rdata_length: Restrict<u16>) -> ProtoResult<SVCB> {
        let start_index = decoder.index();

        let svc_priority = decoder.read_u16()?.unverified(/*any u16 is valid*/);
        let target_name = Name::read(decoder)?;

        let mut remainder_len = rdata_length
            .map(|len| len as usize)
            .checked_sub(decoder.index() - start_index)
            .map_err(|len| format!("Bad length for RDATA of SVCB: {len}"))?
            .unverified(); // valid len
        let mut svc_params: Vec<(SvcParamKey, SvcParamValue)> = Vec::new();

        // must have at least 4 bytes left for the key and the length
        while remainder_len >= 4 {
            // a 2 octet field containing the SvcParamKey as an integer in
            //      network byte order.  (See Section 14.3.2 for the defined values.)
            let key = SvcParamKey::read(decoder)?;

            // a 2 octet field containing the length of the SvcParamValue as an
            //      integer between 0 and 65535 in network byte order (but constrained
            //      by the RDATA and DNS message sizes).
            let value = SvcParamValue::read(key, decoder)?;

            if let Some(last_key) = svc_params.last().map(|(key, _)| key) {
                if last_key >= &key {
                    return Err(ProtoError::from("SvcParams out of order"));
                }
            }

            svc_params.push((key, value));
            remainder_len = rdata_length
                .map(|len| len as usize)
                .checked_sub(decoder.index() - start_index)
                .map_err(|len| format!("Bad length for RDATA of SVCB: {len}"))?
                .unverified(); // valid len
        }

        Ok(Self {
            svc_priority,
            target_name,
            svc_params,
        })
    }
}

impl RecordData for SVCB {
    fn try_borrow(data: &RData) -> Option<&Self> {
        match data {
            RData::SVCB(data) => Some(data),
            _ => None,
        }
    }

    fn record_type(&self) -> RecordType {
        RecordType::SVCB
    }

    fn into_rdata(self) -> RData {
        RData::SVCB(self)
    }
}

/// [RFC 9460 SVCB and HTTPS Resource Records, Nov 2023](https://datatracker.ietf.org/doc/html/rfc9460#section-10.4)
///
/// ```text
/// simple.example. 7200 IN HTTPS 1 . alpn=h3
/// pool  7200 IN HTTPS 1 h3pool alpn=h2,h3 ech="123..."
///               HTTPS 2 .      alpn=h2 ech="abc..."
/// @     7200 IN HTTPS 0 www
/// _8765._baz.api.example.com. 7200 IN SVCB 0 svc4-baz.example.net.
/// ```
impl fmt::Display for SVCB {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(
            f,
            "{svc_priority} {target_name}",
            svc_priority = self.svc_priority,
            target_name = self.target_name,
        )?;

        for (key, param) in self.svc_params.iter() {
            write!(f, " {key}={param}")?
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn read_svcb_key() {
        assert_eq!(SvcParamKey::Mandatory, 0.into());
        assert_eq!(SvcParamKey::Alpn, 1.into());
        assert_eq!(SvcParamKey::NoDefaultAlpn, 2.into());
        assert_eq!(SvcParamKey::Port, 3.into());
        assert_eq!(SvcParamKey::Ipv4Hint, 4.into());
        assert_eq!(SvcParamKey::EchConfigList, 5.into());
        assert_eq!(SvcParamKey::Ipv6Hint, 6.into());
        assert_eq!(SvcParamKey::Key(65280), 65280.into());
        assert_eq!(SvcParamKey::Key(65534), 65534.into());
        assert_eq!(SvcParamKey::Key65535, 65535.into());
        assert_eq!(SvcParamKey::Unknown(65279), 65279.into());
    }

    #[test]
    fn read_svcb_key_to_u16() {
        assert_eq!(u16::from(SvcParamKey::Mandatory), 0);
        assert_eq!(u16::from(SvcParamKey::Alpn), 1);
        assert_eq!(u16::from(SvcParamKey::NoDefaultAlpn), 2);
        assert_eq!(u16::from(SvcParamKey::Port), 3);
        assert_eq!(u16::from(SvcParamKey::Ipv4Hint), 4);
        assert_eq!(u16::from(SvcParamKey::EchConfigList), 5);
        assert_eq!(u16::from(SvcParamKey::Ipv6Hint), 6);
        assert_eq!(u16::from(SvcParamKey::Key(65280)), 65280);
        assert_eq!(u16::from(SvcParamKey::Key(65534)), 65534);
        assert_eq!(u16::from(SvcParamKey::Key65535), 65535);
        assert_eq!(u16::from(SvcParamKey::Unknown(65279)), 65279);
    }

    #[track_caller]
    fn test_encode_decode(rdata: SVCB) {
        let mut bytes = Vec::new();
        let mut encoder = BinEncoder::new(&mut bytes);
        rdata.emit(&mut encoder).expect("failed to emit SVCB");
        let bytes = encoder.into_bytes();

        let mut decoder = BinDecoder::new(bytes);
        let read_rdata = SVCB::read_data(&mut decoder, Restrict::new(bytes.len() as u16))
            .expect("failed to read back");
        assert_eq!(rdata, read_rdata);
    }

    #[test]
    fn test_encode_decode_svcb() {
        test_encode_decode(SVCB::new(
            0,
            Name::from_utf8("www.example.com.").unwrap(),
            vec![],
        ));
        test_encode_decode(SVCB::new(
            0,
            Name::from_utf8(".").unwrap(),
            vec![(
                SvcParamKey::Alpn,
                SvcParamValue::Alpn(Alpn(vec!["h2".to_string()])),
            )],
        ));
        test_encode_decode(SVCB::new(
            0,
            Name::from_utf8("example.com.").unwrap(),
            vec![
                (
                    SvcParamKey::Mandatory,
                    SvcParamValue::Mandatory(Mandatory(vec![SvcParamKey::Alpn])),
                ),
                (
                    SvcParamKey::Alpn,
                    SvcParamValue::Alpn(Alpn(vec!["h2".to_string()])),
                ),
            ],
        ));
    }

    #[test]
    #[should_panic]
    fn test_encode_decode_svcb_bad_order() {
        test_encode_decode(SVCB::new(
            0,
            Name::from_utf8(".").unwrap(),
            vec![
                (
                    SvcParamKey::Alpn,
                    SvcParamValue::Alpn(Alpn(vec!["h2".to_string()])),
                ),
                (
                    SvcParamKey::Mandatory,
                    SvcParamValue::Mandatory(Mandatory(vec![SvcParamKey::Alpn])),
                ),
            ],
        ));
    }

    #[test]
    fn test_no_panic() {
        const BUF: &[u8] = &[
            255, 121, 0, 0, 0, 0, 40, 255, 255, 160, 160, 0, 0, 0, 64, 0, 1, 255, 158, 0, 0, 0, 8,
            0, 0, 7, 7, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert!(crate::op::Message::from_vec(BUF).is_err());
    }

    #[test]
    fn test_unrestricted_output_size() {
        let svcb = SVCB::new(
            8224,
            Name::from_utf8(".").unwrap(),
            vec![(
                SvcParamKey::Unknown(8224),
                SvcParamValue::Unknown(Unknown(vec![32; 257])),
            )],
        );

        let mut buf = Vec::new();
        let mut encoder = BinEncoder::new(&mut buf);
        svcb.emit(&mut encoder).unwrap();
    }

    #[test]
    fn test_unknown_value_round_trip() {
        let svcb = SVCB::new(
            8224,
            Name::from_utf8(".").unwrap(),
            vec![(
                SvcParamKey::Unknown(8224),
                SvcParamValue::Unknown(Unknown(vec![32; 10])),
            )],
        );

        let mut buf = Vec::new();
        let mut encoder = BinEncoder::new(&mut buf);
        svcb.emit(&mut encoder).unwrap();

        let mut decoder = BinDecoder::new(&buf);
        let decoded = SVCB::read_data(
            &mut decoder,
            Restrict::new(u16::try_from(buf.len()).unwrap()),
        )
        .unwrap();

        assert_eq!(svcb, decoded);
    }
}
