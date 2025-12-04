use std::collections::HashMap;
use std::error::Error;
use std::future::Future;
use std::net::{
  Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, ToSocketAddrs,
};
use std::pin::Pin;
use std::str::FromStr;
use std::task::{self, Poll};
use std::{fmt, io, vec};

use hyper_util::client::legacy::connect::dns::Name;
use tokio::task::JoinHandle;
use tower_http::decompression::DecompressionBody;
use tower_service::Service;

/// A resolver using blocking `getaddrinfo` calls in a threadpool.
#[derive(Clone)]
pub struct CustomResolver {
  _priv: (),
  overrides: HashMap<String, Vec<SocketAddr>>,
}

/// An iterator of IP addresses returned from `getaddrinfo`.
pub struct CustomAddrs {
  inner: SocketAddrs,
}

/// A future to resolve a name returned by `CustomResolver`.
pub struct CustomResolverFuture {
  inner: JoinHandle<Result<SocketAddrs, io::Error>>,
}

/// Error indicating a given string was not a valid domain name.
#[derive(Debug)]
pub struct InvalidNameError(());

impl fmt::Display for InvalidNameError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("Not a valid domain name")
  }
}

impl Error for InvalidNameError {}

impl CustomResolver {
  /// Construct a new `CustomResolver`.
  pub fn new() -> Self {
    CustomResolver {
      _priv: (),
      overrides: Default::default(),
    }
  }

  pub fn with_overrides(overrides: HashMap<String, Vec<SocketAddr>>) -> Self {
    Self {
      _priv: (),
      overrides,
    }
  }
}

impl Service<Name> for CustomResolver {
  type Response = CustomAddrs;
  type Error = io::Error;
  type Future = CustomResolverFuture;

  fn poll_ready(
    &mut self,
    _cx: &mut task::Context<'_>,
  ) -> Poll<Result<(), io::Error>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, name: Name) -> Self::Future {
    if let Some(addrs) = self.overrides.get(name.as_str()) {
      let addrs = addrs.clone();
      return CustomResolverFuture {
        inner: tokio::spawn(async {
          Ok(SocketAddrs {
            iter: addrs.into_iter(),
          })
        }),
      };
    }

    let blocking = tokio::task::spawn_blocking(move || {
      // debug!("resolving host={:?}", name.host);
      (name.as_str(), 0)
        .to_socket_addrs()
        .map(|i| SocketAddrs { iter: i })
    });

    CustomResolverFuture { inner: blocking }
  }
}

impl fmt::Debug for CustomResolver {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.pad("CustomResolver")
  }
}

impl Future for CustomResolverFuture {
  type Output = Result<CustomAddrs, io::Error>;

  fn poll(
    mut self: Pin<&mut Self>,
    cx: &mut task::Context<'_>,
  ) -> Poll<Self::Output> {
    Pin::new(&mut self.inner).poll(cx).map(|res| match res {
      Ok(Ok(addrs)) => Ok(CustomAddrs { inner: addrs }),
      Ok(Err(err)) => Err(err),
      Err(join_err) => {
        if join_err.is_cancelled() {
          Err(io::Error::new(io::ErrorKind::Interrupted, join_err))
        } else {
          panic!("gai background task failed: {:?}", join_err)
        }
      }
    })
  }
}

impl fmt::Debug for CustomResolverFuture {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.pad("CustomResolverFuture")
  }
}

impl Drop for CustomResolverFuture {
  fn drop(&mut self) {
    self.inner.abort();
  }
}

impl Iterator for CustomAddrs {
  type Item = SocketAddr;

  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next()
  }
}

impl fmt::Debug for CustomAddrs {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.pad("CustomAddrs")
  }
}

pub(super) struct SocketAddrs {
  iter: vec::IntoIter<SocketAddr>,
}

impl SocketAddrs {
  pub(super) fn new(addrs: Vec<SocketAddr>) -> Self {
    SocketAddrs {
      iter: addrs.into_iter(),
    }
  }

  pub(super) fn try_parse(host: &str, port: u16) -> Option<SocketAddrs> {
    if let Ok(addr) = host.parse::<Ipv4Addr>() {
      let addr = SocketAddrV4::new(addr, port);
      return Some(SocketAddrs {
        iter: vec![SocketAddr::V4(addr)].into_iter(),
      });
    }
    if let Ok(addr) = host.parse::<Ipv6Addr>() {
      let addr = SocketAddrV6::new(addr, port, 0, 0);
      return Some(SocketAddrs {
        iter: vec![SocketAddr::V6(addr)].into_iter(),
      });
    }
    None
  }

  #[inline]
  fn filter(self, predicate: impl FnMut(&SocketAddr) -> bool) -> SocketAddrs {
    SocketAddrs::new(self.iter.filter(predicate).collect())
  }

  pub(super) fn split_by_preference(
    self,
    local_addr_ipv4: Option<Ipv4Addr>,
    local_addr_ipv6: Option<Ipv6Addr>,
  ) -> (SocketAddrs, SocketAddrs) {
    match (local_addr_ipv4, local_addr_ipv6) {
      (Some(_), None) => {
        (self.filter(SocketAddr::is_ipv4), SocketAddrs::new(vec![]))
      }
      (None, Some(_)) => {
        (self.filter(SocketAddr::is_ipv6), SocketAddrs::new(vec![]))
      }
      _ => {
        let preferring_v6 = self
          .iter
          .as_slice()
          .first()
          .map(SocketAddr::is_ipv6)
          .unwrap_or(false);

        let (preferred, fallback) = self
          .iter
          .partition::<Vec<_>, _>(|addr| addr.is_ipv6() == preferring_v6);

        (SocketAddrs::new(preferred), SocketAddrs::new(fallback))
      }
    }
  }

  pub(super) fn is_empty(&self) -> bool {
    self.iter.as_slice().is_empty()
  }

  pub(super) fn len(&self) -> usize {
    self.iter.as_slice().len()
  }
}

impl Iterator for SocketAddrs {
  type Item = SocketAddr;
  #[inline]
  fn next(&mut self) -> Option<SocketAddr> {
    self.iter.next()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::net::{Ipv4Addr, Ipv6Addr};

  #[test]
  fn test_ip_addrs_split_by_preference() {
    let ip_v4 = Ipv4Addr::new(127, 0, 0, 1);
    let ip_v6 = Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1);
    let v4_addr = (ip_v4, 80).into();
    let v6_addr = (ip_v6, 80).into();

    let (mut preferred, mut fallback) = SocketAddrs {
      iter: vec![v4_addr, v6_addr].into_iter(),
    }
    .split_by_preference(None, None);
    assert!(preferred.next().unwrap().is_ipv4());
    assert!(fallback.next().unwrap().is_ipv6());

    let (mut preferred, mut fallback) = SocketAddrs {
      iter: vec![v6_addr, v4_addr].into_iter(),
    }
    .split_by_preference(None, None);
    assert!(preferred.next().unwrap().is_ipv6());
    assert!(fallback.next().unwrap().is_ipv4());

    let (mut preferred, mut fallback) = SocketAddrs {
      iter: vec![v4_addr, v6_addr].into_iter(),
    }
    .split_by_preference(Some(ip_v4), Some(ip_v6));
    assert!(preferred.next().unwrap().is_ipv4());
    assert!(fallback.next().unwrap().is_ipv6());

    let (mut preferred, mut fallback) = SocketAddrs {
      iter: vec![v6_addr, v4_addr].into_iter(),
    }
    .split_by_preference(Some(ip_v4), Some(ip_v6));
    assert!(preferred.next().unwrap().is_ipv6());
    assert!(fallback.next().unwrap().is_ipv4());

    let (mut preferred, fallback) = SocketAddrs {
      iter: vec![v4_addr, v6_addr].into_iter(),
    }
    .split_by_preference(Some(ip_v4), None);
    assert!(preferred.next().unwrap().is_ipv4());
    assert!(fallback.is_empty());

    let (mut preferred, fallback) = SocketAddrs {
      iter: vec![v4_addr, v6_addr].into_iter(),
    }
    .split_by_preference(None, Some(ip_v6));
    assert!(preferred.next().unwrap().is_ipv6());
    assert!(fallback.is_empty());
  }

  #[test]
  fn test_name_from_str() {
    const DOMAIN: &str = "test.example.com";
    let name = Name::from_str(DOMAIN).expect("Should be a valid domain");
    assert_eq!(name.as_str(), DOMAIN);
    assert_eq!(name.to_string(), DOMAIN);
  }
}
