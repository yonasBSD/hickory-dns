use std::{
    net::*,
    str::FromStr,
    sync::{Arc, Mutex as StdMutex},
};

use hickory_proto::{
    op::Query,
    rr::{DNSClass, Name, RData, Record, RecordType, rdata::A},
    runtime::TokioTime,
    xfer::{DnsExchange, DnsMultiplexer, DnsResponse},
};
use hickory_resolver::{
    Hosts, LookupFuture, caching_client::CachingClient, config::LookupIpStrategy, lookup::Lookup,
    lookup_ip::LookupIpFuture,
};
use hickory_server::{
    authority::{Authority, Catalog},
    store::in_memory::InMemoryAuthority,
};
use test_support::subscribe;

use hickory_integration::{TestClientStream, example_authority::create_example, mock_client::*};

#[tokio::test]
async fn test_lookup() {
    subscribe();
    let authority = create_example();
    let mut catalog = Catalog::new();
    catalog.upsert(authority.origin().clone(), vec![Arc::new(authority)]);

    let (stream, sender) = TestClientStream::new(Arc::new(StdMutex::new(catalog)));
    let dns_conn = DnsMultiplexer::new(stream, sender, None);
    let client = DnsExchange::connect::<_, _, TokioTime>(dns_conn);

    let (client, bg) = client.await.expect("client failed to connect");
    tokio::spawn(bg);

    let lookup = LookupFuture::lookup(
        vec![Name::from_str("www.example.com.").unwrap()],
        RecordType::A,
        Default::default(),
        CachingClient::new(0, client, false),
    );
    let lookup = lookup.await.unwrap();

    assert_eq!(
        *lookup.iter().next().unwrap(),
        RData::A(A::new(93, 184, 215, 14))
    );
}

#[tokio::test]
async fn test_lookup_hosts() {
    subscribe();
    let authority = create_example();
    let mut catalog = Catalog::new();
    catalog.upsert(authority.origin().clone(), vec![Arc::new(authority)]);

    let (stream, sender) = TestClientStream::new(Arc::new(StdMutex::new(catalog)));
    let dns_conn = DnsMultiplexer::new(stream, sender, None);

    let client = DnsExchange::connect::<_, _, TokioTime>(dns_conn);
    let (client, bg) = client.await.expect("client connect failed");
    tokio::spawn(bg);

    let mut hosts = Hosts::default();
    let record = Record::from_rdata(
        Name::from_str("www.example.com.").unwrap(),
        86400,
        RData::A(A::new(10, 0, 1, 104)),
    );
    hosts.insert(
        Name::from_str("www.example.com.").unwrap(),
        RecordType::A,
        Lookup::new_with_max_ttl(
            Query::query(Name::from_str("www.example.com.").unwrap(), RecordType::A),
            Arc::from([record]),
        ),
    );

    let lookup = LookupIpFuture::lookup(
        vec![Name::from_str("www.example.com.").unwrap()],
        LookupIpStrategy::default(),
        CachingClient::new(0, client, false),
        Default::default(),
        Arc::new(hosts),
        None,
    );
    let lookup = lookup.await.unwrap();

    assert_eq!(lookup.iter().next().unwrap(), Ipv4Addr::new(10, 0, 1, 104));
}

fn create_ip_like_example() -> InMemoryAuthority {
    let mut authority = create_example();
    authority.upsert_mut(
        Record::from_rdata(
            Name::from_str("1.2.3.4.example.com.").unwrap(),
            86400,
            RData::A(A::new(198, 51, 100, 35)),
        )
        .set_dns_class(DNSClass::IN)
        .clone(),
        0,
    );

    authority
}

#[tokio::test]
async fn test_lookup_ipv4_like() {
    subscribe();
    let authority = create_ip_like_example();
    let mut catalog = Catalog::new();
    catalog.upsert(authority.origin().clone(), vec![Arc::new(authority)]);

    let (stream, sender) = TestClientStream::new(Arc::new(StdMutex::new(catalog)));
    let dns_conn = DnsMultiplexer::new(stream, sender, None);

    let client = DnsExchange::connect::<_, _, TokioTime>(dns_conn);
    let (client, bg) = client.await.expect("client connect failed");
    tokio::spawn(bg);

    let lookup = LookupIpFuture::lookup(
        vec![Name::from_str("1.2.3.4.example.com.").unwrap()],
        LookupIpStrategy::default(),
        CachingClient::new(0, client, false),
        Default::default(),
        Arc::new(Hosts::default()),
        Some(RData::A(A::new(1, 2, 3, 4))),
    );
    let lookup = lookup.await.unwrap();

    assert_eq!(
        lookup.iter().next().unwrap(),
        Ipv4Addr::new(198, 51, 100, 35)
    );
}

#[tokio::test]
async fn test_lookup_ipv4_like_fall_through() {
    subscribe();
    let authority = create_ip_like_example();
    let mut catalog = Catalog::new();
    catalog.upsert(authority.origin().clone(), vec![Arc::new(authority)]);

    let (stream, sender) = TestClientStream::new(Arc::new(StdMutex::new(catalog)));
    let dns_conn = DnsMultiplexer::new(stream, sender, None);

    let client = DnsExchange::connect::<_, _, TokioTime>(dns_conn);
    let (client, bg) = client.await.expect("client connect failed");
    tokio::spawn(bg);

    let lookup = LookupIpFuture::lookup(
        vec![Name::from_str("198.51.100.35.example.com.").unwrap()],
        LookupIpStrategy::default(),
        CachingClient::new(0, client, false),
        Default::default(),
        Arc::new(Hosts::default()),
        Some(RData::A(A::new(198, 51, 100, 35))),
    );
    let lookup = lookup.await.unwrap();

    assert_eq!(
        lookup.iter().next().unwrap(),
        Ipv4Addr::new(198, 51, 100, 35)
    );
}

#[tokio::test]
async fn test_mock_lookup() {
    subscribe();
    let resp_query = Query::query(Name::from_str("www.example.com.").unwrap(), RecordType::A);
    let v4_record = v4_record(
        Name::from_str("www.example.com.").unwrap(),
        Ipv4Addr::new(93, 184, 215, 14),
    );
    let message = message(resp_query, vec![v4_record], vec![], vec![]);
    let client: MockClientHandle<_> =
        MockClientHandle::mock(vec![Ok(DnsResponse::from_message(message).unwrap())]);

    let lookup = LookupFuture::lookup(
        vec![Name::from_str("www.example.com.").unwrap()],
        RecordType::A,
        Default::default(),
        CachingClient::new(0, client, false),
    );

    let lookup = lookup.await.unwrap();

    assert_eq!(
        *lookup.iter().next().unwrap(),
        RData::A(A::new(93, 184, 215, 14))
    );
}

#[tokio::test]
async fn test_cname_lookup() {
    subscribe();
    let resp_query = Query::query(Name::from_str("www.example.com.").unwrap(), RecordType::A);
    let cname_record = cname_record(
        Name::from_str("www.example.com.").unwrap(),
        Name::from_str("v4.example.com.").unwrap(),
    );
    let v4_record = v4_record(
        Name::from_str("v4.example.com.").unwrap(),
        Ipv4Addr::new(93, 184, 215, 14),
    );
    let message = message(resp_query, vec![cname_record, v4_record], vec![], vec![]);
    let client: MockClientHandle<_> =
        MockClientHandle::mock(vec![Ok(DnsResponse::from_message(message).unwrap())]);

    let lookup = LookupFuture::lookup(
        vec![Name::from_str("www.example.com.").unwrap()],
        RecordType::A,
        Default::default(),
        CachingClient::new(0, client, false),
    );

    let lookup = lookup.await.unwrap();

    assert_eq!(
        *lookup.iter().next().unwrap(),
        RData::A(A::new(93, 184, 215, 14))
    );
}

#[tokio::test]
async fn test_cname_lookup_preserve() {
    subscribe();
    let resp_query = Query::query(Name::from_str("www.example.com.").unwrap(), RecordType::A);
    let cname_record = cname_record(
        Name::from_str("www.example.com.").unwrap(),
        Name::from_str("v4.example.com.").unwrap(),
    );
    let v4_record = v4_record(
        Name::from_str("v4.example.com.").unwrap(),
        Ipv4Addr::new(93, 184, 215, 14),
    );
    let message = message(
        resp_query,
        vec![cname_record.clone(), v4_record],
        vec![],
        vec![],
    );
    let client: MockClientHandle<_> =
        MockClientHandle::mock(vec![Ok(DnsResponse::from_message(message).unwrap())]);

    let lookup = LookupFuture::lookup(
        vec![Name::from_str("www.example.com.").unwrap()],
        RecordType::A,
        Default::default(),
        CachingClient::new(0, client, true),
    );

    let lookup = lookup.await.unwrap();

    let mut iter = lookup.iter();
    assert_eq!(iter.next().unwrap(), cname_record.data());
    assert_eq!(*iter.next().unwrap(), RData::A(A::new(93, 184, 215, 14)));
}

#[tokio::test]
async fn test_chained_cname_lookup() {
    subscribe();
    let resp_query = Query::query(Name::from_str("www.example.com.").unwrap(), RecordType::A);
    let cname_record = cname_record(
        Name::from_str("www.example.com.").unwrap(),
        Name::from_str("v4.example.com.").unwrap(),
    );
    let v4_record = v4_record(
        Name::from_str("v4.example.com.").unwrap(),
        Ipv4Addr::new(93, 184, 215, 14),
    );

    // The first response should be a cname, the second will be the actual record
    let message1 = message(resp_query.clone(), vec![cname_record], vec![], vec![]);
    let message2 = message(resp_query, vec![v4_record], vec![], vec![]);

    // the mock pops messages...
    let client: MockClientHandle<_> = MockClientHandle::mock(vec![
        Ok(DnsResponse::from_message(message2).unwrap()),
        Ok(DnsResponse::from_message(message1).unwrap()),
    ]);

    let lookup = LookupFuture::lookup(
        vec![Name::from_str("www.example.com.").unwrap()],
        RecordType::A,
        Default::default(),
        CachingClient::new(0, client, false),
    );

    let lookup = lookup.await.unwrap();

    assert_eq!(
        *lookup.iter().next().unwrap(),
        RData::A(A::new(93, 184, 215, 14))
    );
}

#[tokio::test]
async fn test_chained_cname_lookup_preserve() {
    subscribe();
    let resp_query = Query::query(Name::from_str("www.example.com.").unwrap(), RecordType::A);
    let cname_record = cname_record(
        Name::from_str("www.example.com.").unwrap(),
        Name::from_str("v4.example.com.").unwrap(),
    );
    let v4_record = v4_record(
        Name::from_str("v4.example.com.").unwrap(),
        Ipv4Addr::new(93, 184, 215, 14),
    );

    // The first response should be a cname, the second will be the actual record
    let message1 = message(
        resp_query.clone(),
        vec![cname_record.clone()],
        vec![],
        vec![],
    );
    let message2 = message(resp_query, vec![v4_record], vec![], vec![]);

    // the mock pops messages...
    let client: MockClientHandle<_> = MockClientHandle::mock(vec![
        Ok(DnsResponse::from_message(message2).unwrap()),
        Ok(DnsResponse::from_message(message1).unwrap()),
    ]);

    let lookup = LookupFuture::lookup(
        vec![Name::from_str("www.example.com.").unwrap()],
        RecordType::A,
        Default::default(),
        CachingClient::new(0, client, true),
    );

    let lookup = lookup.await.unwrap();

    let mut iter = lookup.iter();
    assert_eq!(iter.next().unwrap(), cname_record.data());
    assert_eq!(*iter.next().unwrap(), RData::A(A::new(93, 184, 215, 14)));
}

#[tokio::test]
async fn test_max_chained_lookup_depth() {
    subscribe();
    let resp_query = Query::query(Name::from_str("www.example.com.").unwrap(), RecordType::A);
    let cname_record1 = cname_record(
        Name::from_str("www.example.com.").unwrap(),
        Name::from_str("cname2.example.com.").unwrap(),
    );
    let cname_record2 = cname_record(
        Name::from_str("cname2.example.com.").unwrap(),
        Name::from_str("cname3.example.com.").unwrap(),
    );
    let cname_record3 = cname_record(
        Name::from_str("cname3.example.com.").unwrap(),
        Name::from_str("cname4.example.com.").unwrap(),
    );
    let cname_record4 = cname_record(
        Name::from_str("cname4.example.com.").unwrap(),
        Name::from_str("cname5.example.com.").unwrap(),
    );
    let cname_record5 = cname_record(
        Name::from_str("cname5.example.com.").unwrap(),
        Name::from_str("cname6.example.com.").unwrap(),
    );
    let cname_record6 = cname_record(
        Name::from_str("cname6.example.com.").unwrap(),
        Name::from_str("cname7.example.com.").unwrap(),
    );
    let cname_record7 = cname_record(
        Name::from_str("cname7.example.com.").unwrap(),
        Name::from_str("cname8.example.com.").unwrap(),
    );
    let cname_record8 = cname_record(
        Name::from_str("cname8.example.com.").unwrap(),
        Name::from_str("cname9.example.com.").unwrap(),
    );
    let cname_record9 = cname_record(
        Name::from_str("cname9.example.com.").unwrap(),
        Name::from_str("v4.example.com.").unwrap(),
    );
    let v4_record = v4_record(
        Name::from_str("v4.example.com.").unwrap(),
        Ipv4Addr::new(93, 184, 215, 14),
    );

    // The first response should be a cname, the second will be the actual record
    let message1 = message(resp_query.clone(), vec![cname_record1], vec![], vec![]);
    let message2 = message(resp_query.clone(), vec![cname_record2], vec![], vec![]);
    let message3 = message(resp_query.clone(), vec![cname_record3], vec![], vec![]);
    let message4 = message(resp_query.clone(), vec![cname_record4], vec![], vec![]);
    let message5 = message(resp_query.clone(), vec![cname_record5], vec![], vec![]);
    let message6 = message(resp_query.clone(), vec![cname_record6], vec![], vec![]);
    let message7 = message(resp_query.clone(), vec![cname_record7], vec![], vec![]);
    let message8 = message(resp_query.clone(), vec![cname_record8], vec![], vec![]);
    let message9 = message(resp_query.clone(), vec![cname_record9], vec![], vec![]);
    let message10 = message(resp_query, vec![v4_record], vec![], vec![]);

    // the mock pops messages...
    let client: MockClientHandle<_> = MockClientHandle::mock(vec![
        Ok(DnsResponse::from_message(message10).unwrap()),
        Ok(DnsResponse::from_message(message9).unwrap()),
        Ok(DnsResponse::from_message(message8).unwrap()),
        Ok(DnsResponse::from_message(message7).unwrap()),
        Ok(DnsResponse::from_message(message6).unwrap()),
        Ok(DnsResponse::from_message(message5).unwrap()),
        Ok(DnsResponse::from_message(message4).unwrap()),
        Ok(DnsResponse::from_message(message3).unwrap()),
        Ok(DnsResponse::from_message(message2).unwrap()),
        Ok(DnsResponse::from_message(message1).unwrap()),
    ]);

    let client = CachingClient::new(0, client, false);
    let lookup = LookupFuture::lookup(
        vec![Name::from_str("www.example.com.").unwrap()],
        RecordType::A,
        Default::default(),
        client.clone(),
    );

    println!("performing max cname validation");
    assert!(lookup.await.is_err());

    // This query should succeed, as the queue depth should reset to 0 on a failed request
    let lookup = LookupFuture::lookup(
        vec![Name::from_str("cname9.example.com.").unwrap()],
        RecordType::A,
        Default::default(),
        client,
    );

    println!("performing followup resolve, should work");
    let lookup = lookup.await.unwrap();

    assert_eq!(
        *lookup.iter().next().unwrap(),
        RData::A(A::new(93, 184, 215, 14))
    );
}

// This test expects a no-answer query which returns a SOA record in the nameservers section to
// contain a ProtoErrorKind::NoRecordsFound error with a SOA record present (soa.is_some()) and
// no NS records present (!ns.is_some())
#[tokio::test]
async fn test_forward_soa() {
    use hickory_proto::ProtoErrorKind;

    subscribe();
    let resp_query = Query::query(Name::from_str("www.example.com.").unwrap(), RecordType::NS);
    let soa_record = soa_record(
        Name::from_str("www.example.com").unwrap(),
        Name::from_str("ns1.example.com").unwrap(),
    );
    let message = message(resp_query.clone(), vec![], vec![soa_record], vec![]);

    let client: MockClientHandle<_> =
        MockClientHandle::mock(vec![Ok(DnsResponse::from_message(message).unwrap())]);

    let client = CachingClient::new(0, client, false);
    let lookup = LookupFuture::lookup(
        vec![Name::from_str("www.example.com.").unwrap()],
        RecordType::NS,
        Default::default(),
        client.clone(),
    );

    let lookup = lookup.await;
    let Err(e) = lookup else {
        panic!("Expected Error type for {lookup:?}");
    };

    let ProtoErrorKind::NoRecordsFound(no_records) = e.kind() else {
        panic!("Unexpected kind: {e:?}");
    };

    assert!(no_records.soa.is_some());
    assert!(no_records.ns.is_none());
}

// This test expects a no-answer query which returns an NS record in the nameservers section to
// contain a ProtoErrorKind::NoRecordsFound error with an NS record present (ns.is_some()) and
// no SOA records present (!soa.is_some())
#[tokio::test]
async fn test_forward_ns() {
    use hickory_proto::ProtoErrorKind;

    subscribe();
    let resp_query = Query::query(Name::from_str("example.com.").unwrap(), RecordType::A);
    let ns1 = ns_record(
        Default::default(),
        Name::from_str("ns1.example.com").unwrap(),
    );
    let message = message(resp_query.clone(), vec![], vec![ns1], vec![]);

    let client: MockClientHandle<_> =
        MockClientHandle::mock(vec![Ok(DnsResponse::from_message(message).unwrap())]);

    let client = CachingClient::new(0, client, false);
    let lookup = LookupFuture::lookup(
        vec![Name::from_str("example.com.").unwrap()],
        RecordType::A,
        Default::default(),
        client.clone(),
    );

    let lookup = lookup.await;
    let Err(e) = lookup else {
        panic!("Expected Error type for {lookup:?}");
    };

    let ProtoErrorKind::NoRecordsFound(no_records) = e.kind() else {
        panic!("Unexpected kind: {e:?}");
    };

    assert!(no_records.soa.is_none());
    assert!(no_records.ns.is_some());
}
