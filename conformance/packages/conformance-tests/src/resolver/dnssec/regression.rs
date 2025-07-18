use std::{net::Ipv4Addr, time::Duration};

use dns_test::{
    FQDN, Implementation, Network, Resolver, Result,
    client::{Client, DigSettings},
    name_server::{Graph, NameServer, Sign},
    record::{A, Record, RecordType},
    tshark::{Capture, Direction},
    zone_file::SignSettings,
};

/// regression test for https://github.com/hickory-dns/hickory-dns/issues/2299
#[test]
fn includes_rrsig_record_in_ns_query() -> Result<()> {
    let network = Network::new()?;
    let leaf_ns = NameServer::new(&dns_test::PEER, FQDN::TEST_DOMAIN, &network)?;

    let Graph {
        nameservers: _nameservers,
        root,
        trust_anchor: _trust_anchor,
    } = Graph::build(
        leaf_ns,
        Sign::Yes {
            settings: SignSettings::default(),
        },
    )?;

    // NOTE this is a security-aware, *non*-validating resolver
    let resolver = Resolver::new(&network, root).start()?;

    let resolver_addr = resolver.ipv4_addr();

    let client = Client::new(resolver.network())?;

    let output = client.dig(
        *DigSettings::default().dnssec().recurse(),
        resolver_addr,
        RecordType::NS,
        &FQDN::TEST_DOMAIN,
    )?;

    assert!(output.status.is_noerror());

    // bug: this answer was missing the `rrsig` record
    let [ns, rrsig] = output.answer.try_into().unwrap();

    // check that we got the expected record types
    assert!(matches!(ns, Record::NS(_)));
    let rrsig = rrsig.try_into_rrsig().unwrap();
    assert_eq!(RecordType::NS, rrsig.type_covered);

    Ok(())
}

/// This is a regression test for https://github.com/hickory-dns/hickory-dns/issues/2285
#[test]
fn can_validate_ns_query() -> Result<()> {
    let network = Network::new()?;
    let leaf_ns = NameServer::new(&dns_test::PEER, FQDN::TEST_DOMAIN, &network)?;

    let Graph {
        nameservers: _nameservers,
        root,
        trust_anchor,
    } = Graph::build(
        leaf_ns,
        Sign::Yes {
            settings: SignSettings::default(),
        },
    )?;

    // NOTE this is a security-aware, *validating* resolver
    let resolver = Resolver::new(&network, root)
        .trust_anchor(&trust_anchor.unwrap())
        .start()?;

    let resolver_addr = resolver.ipv4_addr();

    let client = Client::new(resolver.network())?;

    let output = client.dig(
        *DigSettings::default().authentic_data().recurse(),
        resolver_addr,
        RecordType::NS,
        &FQDN::TEST_DOMAIN,
    )?;

    // bug: this returned SERVFAIL instead of NOERROR with AD=1
    assert!(output.status.is_noerror());
    assert!(output.flags.authenticated_data);

    // check that the record type is what we expect
    let [ns] = output.answer.try_into().unwrap();
    assert!(matches!(ns, Record::NS(_)));

    Ok(())
}

#[test]
fn can_validate_ns_query_case_randomization() -> Result<()> {
    let network = Network::new()?;
    let leaf_ns = NameServer::new(&dns_test::PEER, FQDN::TEST_DOMAIN, &network)?;

    let Graph {
        nameservers: _nameservers,
        root,
        trust_anchor,
    } = Graph::build(
        leaf_ns,
        Sign::Yes {
            settings: SignSettings::default(),
        },
    )?;

    let resolver = Resolver::new(&network, root)
        .trust_anchor(&trust_anchor.unwrap())
        .case_randomization()
        .start()?;

    let resolver_addr = resolver.ipv4_addr();
    let mut tshark = resolver.eavesdrop()?;

    let client = Client::new(resolver.network())?;

    let output = client.dig(
        *DigSettings::default().authentic_data().recurse(),
        resolver_addr,
        RecordType::NS,
        &FQDN::TEST_DOMAIN,
    )?;

    tshark.wait_until(
        |captures| {
            captures.iter().any(|capture| match capture.direction {
                Direction::Outgoing { destination } => destination == client.ipv4_addr(),
                _ => false,
            })
        },
        Duration::from_secs(10),
    )?;
    let captures = tshark.terminate()?;

    assert!(output.status.is_noerror());
    assert!(output.flags.authenticated_data);

    // check that the record type is what we expect
    let [ns] = output.answer.try_into().unwrap();
    assert!(matches!(ns, Record::NS(_)));

    // check that the resolver returns the original query
    let mut saw_response = false;
    for capture in captures {
        let Direction::Outgoing { destination } = capture.direction else {
            continue;
        };
        if destination != client.ipv4_addr() {
            continue;
        }
        let message_value = capture.message.as_value().as_object().unwrap();
        let queries = message_value.get("Queries").unwrap().as_object().unwrap();
        let query = queries.values().next().unwrap().as_object().unwrap();
        assert_eq!(query.get("dns.qry.name").unwrap(), "hickory-dns.testing");
        saw_response = true;
    }
    assert!(saw_response);

    Ok(())
}

/// regression test for https://github.com/hickory-dns/hickory-dns/issues/2306
#[test]
fn single_node_dns_graph_with_bind_as_peer() -> Result<()> {
    let network = Network::new()?;
    let peer = Implementation::Bind;
    let nameserver = NameServer::new(&peer, FQDN::ROOT, &network)?
        .sign(SignSettings::default())?
        .start()?;

    let client = Client::new(&network)?;

    let nameserver_addr = nameserver.ipv4_addr();
    let ans = client.dig(
        DigSettings::default(),
        nameserver_addr,
        RecordType::NS,
        &FQDN::ROOT,
    )?;

    // sanity check
    assert!(ans.status.is_noerror());
    let [ns] = ans.answer.try_into().unwrap();
    assert!(matches!(ns, Record::NS(_)));

    // pre-condition: BIND does NOT include a glue record (A record) in the additional section
    assert!(ans.additional.is_empty());

    let resolver = Resolver::new(&network, nameserver.root_hint()).start()?;

    let mut tshark = resolver.eavesdrop()?;

    let ans = client.dig(
        *DigSettings::default().recurse(),
        resolver.ipv4_addr(),
        RecordType::SOA,
        &FQDN::ROOT,
    )?;

    tshark.wait_for_capture()?;
    let captures = tshark.terminate()?;

    dbg!(captures.len());

    assert!(ans.status.is_noerror());

    let [soa] = ans.answer.try_into().unwrap();
    assert!(matches!(soa, Record::SOA(_)));

    // bug: hickory-dns goes into an infinite loop until it exhausts its network resources
    assert!(captures.len() < 20);

    for Capture { message, direction } in captures {
        if let Direction::Outgoing { destination } = direction {
            if destination == nameserver_addr {
                eprintln!("{message:#?}\n");
            }
        }
    }

    Ok(())
}

#[test]
fn five_secure_zones() -> Result<()> {
    let network = Network::new()?;

    let subdomain_zone = FQDN::TEST_DOMAIN.push_label("bar");
    let leaf_zone = subdomain_zone.push_label("foo");

    let mut leaf_ns = NameServer::new(&dns_test::PEER, leaf_zone.clone(), &network)?;
    leaf_ns.add(A {
        fqdn: leaf_zone.clone(),
        ttl: 86400,
        ipv4_addr: Ipv4Addr::new(1, 2, 3, 4),
    });
    let leaf_ns = leaf_ns.sign(SignSettings::default())?;

    let mut subdomain_ns = NameServer::new(&dns_test::PEER, subdomain_zone.clone(), &network)?;
    subdomain_ns.referral_nameserver(&leaf_ns);
    subdomain_ns.add(leaf_ns.ds().ksk.clone());
    let subdomain_ns = subdomain_ns.sign(SignSettings::default())?;

    let mut domain_ns = NameServer::new(&dns_test::PEER, FQDN::TEST_DOMAIN, &network)?;
    domain_ns.referral_nameserver(&subdomain_ns);
    domain_ns.add(subdomain_ns.ds().ksk.clone());
    let domain_ns = domain_ns.sign(SignSettings::default())?;

    let mut tld_ns = NameServer::new(&dns_test::PEER, FQDN::TEST_TLD, &network)?;
    tld_ns.referral_nameserver(&domain_ns);
    tld_ns.add(domain_ns.ds().ksk.clone());
    let tld_ns = tld_ns.sign(SignSettings::default())?;

    let mut root_ns = NameServer::new(&dns_test::PEER, FQDN::ROOT, &network)?;
    root_ns.referral_nameserver(&tld_ns);
    root_ns.add(tld_ns.ds().ksk.clone());
    let root_ns = root_ns.sign(SignSettings::default())?;
    let root = root_ns.root_hint();
    let trust_anchor = root_ns.trust_anchor();

    let _root_ns = root_ns.start()?;
    let _tld_ns = tld_ns.start()?;
    let _domain_ns = domain_ns.start()?;
    let _subdomain_ns = subdomain_ns.start()?;
    let _leaf_ns = leaf_ns.start()?;

    let resolver = Resolver::new(&network, root)
        .trust_anchor(&trust_anchor)
        .start()?;

    let client = Client::new(&network)?;

    let output = client.dig(
        *DigSettings::default().recurse(),
        resolver.ipv4_addr(),
        RecordType::A,
        &leaf_zone,
    )?;

    assert!(output.status.is_noerror());
    assert!(!output.answer.is_empty());

    Ok(())
}

#[test]
fn glue_reuse() -> Result<()> {
    let network = Network::new()?;

    let sibling_zone = FQDN::TEST_TLD.push_label("sibling");

    let leaf_ns = NameServer::builder(dns_test::PEER.clone(), FQDN::TEST_DOMAIN, network.clone())
        .nameserver_fqdn(sibling_zone.clone())
        .build()?;
    let leaf_ns = leaf_ns.sign(SignSettings::default())?;

    let mut sibling_ns = NameServer::new(&dns_test::PEER, sibling_zone.clone(), &network)?;
    sibling_ns.add(Record::a(sibling_zone.clone(), leaf_ns.ipv4_addr()));
    let sibling_ns = sibling_ns.sign(SignSettings::default())?;

    let mut tld_ns = NameServer::new(&dns_test::PEER, FQDN::TEST_TLD, &network)?;
    tld_ns.referral_nameserver(&leaf_ns);
    tld_ns.add(leaf_ns.ds().ksk.clone());
    tld_ns.referral_nameserver(&sibling_ns);
    tld_ns.add(sibling_ns.ds().ksk.clone());
    let tld_ns = tld_ns.sign(SignSettings::default())?;

    let mut root_ns = NameServer::new(&dns_test::PEER, FQDN::ROOT, &network)?;
    root_ns.referral_nameserver(&tld_ns);
    root_ns.add(tld_ns.ds().ksk.clone());
    let root_ns = root_ns.sign(SignSettings::default())?;
    let root = root_ns.root_hint();
    let trust_anchor = root_ns.trust_anchor();

    let _root_ns = root_ns.start()?;
    let _tld_ns = tld_ns.start()?;
    let _sibling_ns = sibling_ns.start()?;
    let _leaf_ns = leaf_ns.start()?;

    let resolver = Resolver::new(&network, root)
        .trust_anchor(&trust_anchor)
        .start()?;
    let client = Client::new(&network)?;
    let settings = *DigSettings::default().recurse();

    // Make a query to prime the cache with a glue RRset.
    client.dig(
        settings,
        resolver.ipv4_addr(),
        RecordType::NS,
        &FQDN::TEST_DOMAIN,
    )?;

    // Make an A query, and see if it reuses the glue RRset that was stored previously.
    let output = client.dig(settings, resolver.ipv4_addr(), RecordType::A, &sibling_zone)?;

    assert!(output.status.is_noerror(), "{:?}", output.status);
    assert!(!output.answer.is_empty());

    Ok(())
}
