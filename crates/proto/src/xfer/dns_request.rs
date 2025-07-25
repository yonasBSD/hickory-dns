// Copyright 2015-2018 Benjamin Fry <benjaminfry@me.com>
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// https://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// https://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! `DnsRequest` wraps a `Message` and associates a set of `DnsRequestOptions` for specifying different transfer options.

use core::ops::{Deref, DerefMut};

use crate::op::{Message, Query};

/// A set of options for expressing options to how requests should be treated
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DnsRequestOptions {
    // TODO: add EDNS options here?
    /// When true, will add EDNS options to the request.
    pub use_edns: bool,
    /// When true, sets the DO bit in the EDNS options
    pub edns_set_dnssec_ok: bool,
    /// Specifies maximum request depth for DNSSEC validation.
    pub max_request_depth: usize,
    /// set recursion desired (or not) for any requests
    pub recursion_desired: bool,
    /// Randomize case of query name, and check that the response matches, for spoofing resistance.
    #[cfg(feature = "std")]
    pub case_randomization: bool,
}

impl Default for DnsRequestOptions {
    fn default() -> Self {
        Self {
            max_request_depth: 26,
            use_edns: false,
            edns_set_dnssec_ok: false,
            recursion_desired: true,
            #[cfg(feature = "std")]
            case_randomization: false,
        }
    }
}

/// A DNS request object
///
/// This wraps a DNS Message for requests. It also has request options associated for controlling certain features of the DNS protocol handlers.
#[derive(Clone, PartialEq, Eq)]
pub struct DnsRequest {
    message: Message,
    options: DnsRequestOptions,
    /// If case randomization was replied to the request, this holds the original query.
    original_query: Option<Query>,
}

impl DnsRequest {
    /// Returns a new DnsRequest object
    pub fn new(message: Message, options: DnsRequestOptions) -> Self {
        Self {
            message,
            options,
            original_query: None,
        }
    }

    /// Add the original query
    pub fn with_original_query(mut self, original_query: Option<Query>) -> Self {
        self.original_query = original_query;
        self
    }

    /// Get the set of request options associated with this request
    pub fn options(&self) -> &DnsRequestOptions {
        &self.options
    }

    /// Unwraps the raw message
    pub fn into_parts(self) -> (Message, DnsRequestOptions) {
        (self.message, self.options)
    }

    /// Get the request's original query
    pub fn original_query(&self) -> Option<&Query> {
        self.original_query.as_ref()
    }
}

impl Deref for DnsRequest {
    type Target = Message;
    fn deref(&self) -> &Self::Target {
        &self.message
    }
}

impl DerefMut for DnsRequest {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.message
    }
}

impl From<Message> for DnsRequest {
    fn from(message: Message) -> Self {
        Self::new(message, DnsRequestOptions::default())
    }
}
