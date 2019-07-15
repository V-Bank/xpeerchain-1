// Copyright (c) The XPeer Core Contributors
// SPDX-License-Identifier: Apache-2.0

//! The ConnectivityManager actor is responsible for ensuring that we are connected to a node
//! if and only if it is an eligible node.
//! A list of eligible nodes is received at initialization, and updates are received on changes
//! to system membership.
//!
//! In our current system design, the Consensus actor informs the ConnectivityManager of
//! eligible nodes, and the Discovery actor infroms it about updates to addresses of eligible
//! nodes.
use crate::{
    common::NetworkPublicKeys,
    peer_manager::{PeerManagerError, PeerManagerNotification, PeerManagerRequestSender},
};
use channel;
use futures::{
    channel::oneshot,
    compat::Future01CompatExt,
    future::{BoxFuture, FutureExt},
    stream::{FusedStream, FuturesUnordered, Stream, StreamExt},
};
use logger::prelude::*;
use parity_multiaddr::Multiaddr;
use std::{
    cmp::min,
    collections::HashMap,
    fmt::Debug,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use tokio::timer;
use types::PeerId;

#[cfg(test)]
mod test;

/// The ConnectivityManager actor.
pub struct ConnectivityManager<TTicker, TSubstream, TBackoff> {
    /// Nodes which are eligible to join the network.
    eligible: Arc<RwLock<HashMap<PeerId, NetworkPublicKeys>>>,
    /// PeerId and address of remote peers to which this peer is connected.
    connected: HashMap<PeerId, Multiaddr>,
    /// Addresses of peers received from Discovery module.
    peer_addresses: HashMap<PeerId, Vec<Multiaddr>>,
    /// Ticker to trigger connectivity checks to provide the guarantees stated above.
    ticker: TTicker,
    /// Channel to send requests to PeerManager.
    peer_mgr_reqs_tx: PeerManagerRequestSender<TSubstream>,
    /// Channel to receive notifications from PeerManager.
    peer_mgr_notifs_rx: channel::Receiver<PeerManagerNotification<TSubstream>>,
    /// Channel over which we receive requests from other actors.
    requests_rx: channel::Receiver<ConnectivityRequest>,
    /// Peers queued to be dialed, potentially with some delay. The dial can be cancelled by
    /// sending over (or dropping) the associated oneshot sender.
    dial_queue: HashMap<PeerId, oneshot::Sender<()>>,
    /// Tracks status of backoff strategy for connection attempts for each peer.
    dial_backoffs: HashMap<PeerId, TBackoff>,
    /// Backoff strategy.
    backoff_strategy: TBackoff,
    /// Maximum delay b/w 2 consecutive attempts to connect with a disconnected peer.
    max_delay_ms: u64,
    /// A local counter incremented on receiving an incoming message. Printing this in debugging
    /// allows for easy debugging.
    event_id: u32,
}

/// Requests received by the [`ConnectivityManager`] manager actor from upstream modules.
#[derive(Debug)]
pub enum ConnectivityRequest {
    /// Request to update known addresses of peer with id `PeerId` to given list.
    UpdateAddresses(PeerId, Vec<Multiaddr>),
    /// Update set of nodes eligible to join the network.
    UpdateEligibleNodes(HashMap<PeerId, NetworkPublicKeys>),
    /// Gets current size of dial queue. This is useful in tests.
    GetDialQueueSize(oneshot::Sender<usize>),
}

#[derive(Debug)]
enum DialResult {
    Success,
    Cancelled,
    Failed(PeerManagerError),
}

impl<TTicker, TSubstream, TBackoff> ConnectivityManager<TTicker, TSubstream, TBackoff>
where
    TTicker: Stream + FusedStream + Unpin + 'static,
    TSubstream: Debug + Send + 'static,
    TBackoff: Iterator<Item = Duration> + Clone,
{
    /// Creates a new instance of the [`ConnectivityManager`] actor.
    pub fn new(
        eligible: Arc<RwLock<HashMap<PeerId, NetworkPublicKeys>>>,
        ticker: TTicker,
        peer_mgr_reqs_tx: PeerManagerRequestSender<TSubstream>,
        peer_mgr_notifs_rx: channel::Receiver<PeerManagerNotification<TSubstream>>,
        requests_rx: channel::Receiver<ConnectivityRequest>,
        backoff_strategy: TBackoff,
        max_delay_ms: u64,
    ) -> Self {
        Self {
            eligible,
            connected: HashMap::new(),
            peer_addresses: HashMap::new(),
            ticker,
            peer_mgr_reqs_tx,
            peer_mgr_notifs_rx,
            requests_rx,
            dial_backoffs: HashMap::new(),
            dial_queue: HashMap::new(),
            event_id: 0,
            backoff_strategy,
            max_delay_ms,
        }
    }

    /// Starts the [`ConnectivityManager`] actor.
    pub async fn start(mut self) {
        // The ConnectivityManager actor is interested in 3 kinds of events:
        // 1. Ticks to trigger connecitvity check. These are implemented using a clock based
        //    trigger in production.
        // 2. Incoming requests to connect or disconnect with a peer.
        // 3. Notifications from PeerManager when we establish a new connection or lose an existing
        //    connection with a peer.
        let mut pending_dials = FuturesUnordered::new();
        loop {
            self.event_id += 1;
            ::futures::select! {
                _ = self.ticker.select_next_some() => {
                    trace!("Event Id: {}, type: Tick", self.event_id);
                    self.check_connectivity(&mut pending_dials).await;
                },
                req = self.requests_rx.select_next_some() => {
                    trace!("Event Id: {}, type: ConnectivityRequest, req: {:?}", self.event_id, req);
                    self.handle_request(req);
                },
                notif = self.peer_mgr_notifs_rx.select_next_some() => {
                    trace!("Event Id: {}, type: PeerManagerNotification, notif: {:?}", self.event_id, notif);
                    self.handle_peer_mgr_notification(notif);
                },
                peer_id = pending_dials.select_next_some() => {
                    trace!("Event Id: {}, type: Dial complete, peer: {}", self.event_id, peer_id.short_str());
                    self.dial_queue.remove(&peer_id);
                },
                complete => {
                    crit!("Connectivity manager actor terminated");
                    break;
                }
            }
        }
    }

    async fn close_stale_connections(&mut self) {
        let eligible = self.eligible.read().unwrap().clone();
        let stale_connections: Vec<_> = self
            .connected
            .keys()
            .filter(|peer_id| !eligible.contains_key(peer_id))
            .cloned()
            .collect();
        for p in stale_connections.into_iter() {
            info!("Should no longer be connected to peer: {}", p.short_str());
            // Close existing connection.
            if let Err(e) = self.peer_mgr_reqs_tx.disconnect_peer(p).await {
                info!(
                    "Failed to disconnect from peer: {}. Error: {:?}",
                    p.short_str(),
                    e
                );
            }
        }
    }

    async fn cancel_stale_dials(&mut self) {
        let eligible = self.eligible.read().unwrap().clone();
        let stale_dials: Vec<_> = self
            .dial_queue
            .keys()
            .filter(|peer_id| !eligible.contains_key(peer_id))
            .cloned()
            .collect();
        for p in stale_dials.into_iter() {
            self.dial_queue.remove(&p);
        }
    }

    async fn dial_eligible_peers<'a>(
        &'a mut self,
        pending_dials: &'a mut FuturesUnordered<BoxFuture<'static, PeerId>>,
    ) {
        let eligible = self.eligible.read().unwrap().clone();
        let to_connect: Vec<_> = self
            .peer_addresses
            .iter()
            .filter(|(peer_id, _)| {
                eligible.contains_key(peer_id)
                    && self.connected.get(peer_id).is_none()
                    && self.dial_queue.get(peer_id).is_none()
            })
            .collect();
        // We tune max delay depending on the number of peers to which we're not connected. This
        // ensures that if we're disconnected from a large fraction of peers, we keep the retry
        // window smaller.
        let max_delay = Duration::from_millis(
            (self.max_delay_ms as f64
                * (1.0
                    - ((self.dial_queue.len() + to_connect.len()) as f64
                        / eligible
                            .iter()
                            .filter(|(peer_id, _)| self.peer_addresses.contains_key(peer_id))
                            .count() as f64))) as u64,
        );
        let default_backoff_strategy = self.backoff_strategy.clone();
        for (p, addrs) in to_connect.into_iter() {
            info!(
                "Should be connected to peer: {} at addr(s): {:?}",
                p.short_str(),
                addrs,
            );
            let mut peer_mgr_reqs_tx = self.peer_mgr_reqs_tx.clone();
            let peer_id = *p;
            let addr = addrs[0].clone();
            let (cancel_tx, cancel_rx) = oneshot::channel();
            let now = Instant::now();
            let delay = timer::Delay::new(
                now.checked_add(min(
                    max_delay,
                    self.dial_backoffs
                        .entry(peer_id)
                        .or_insert_with(|| default_backoff_strategy.clone())
                        .next()
                        .unwrap_or(max_delay),
                ))
                .unwrap_or_else(Instant::now),
            );
            // Create future which completes by either dialing after calculated delay or on
            // cancellation.
            let f = async move {
                info!(
                    "Will dial peer: {} after {:?}",
                    peer_id.short_str(),
                    delay.deadline().duration_since(now)
                );
                // We dial after a delay. The dial can be cancelled by sending to or dropping
                // `cancel_rx`.
                let dial_result = ::futures::select! {
                    _ = delay.compat().fuse() => {
                        info!("Dialing peer: {} at address: {}", peer_id.short_str(), addr.clone());
                        match peer_mgr_reqs_tx.dial_peer(peer_id, addr.clone()).await {
                            Ok(_) => DialResult::Success,
                            Err(e) => DialResult::Failed(e),
                        }
                    },
                    _ = cancel_rx.fuse() => {
                        DialResult::Cancelled
                    },
                };
                log_dial_result(peer_id, addr, dial_result);
                // Send peer_id as future result so it can be removed from dial queue.
                peer_id
            };
            pending_dials.push(f.boxed());
            self.dial_queue.insert(peer_id, cancel_tx);
        }
    }

    // Note: We do not check that the connections to older incarnations of a node are broken, and
    // instead rely on the node moving to a new epoch to break connections made from older
    // incarnations.
    async fn check_connectivity<'a>(
        &'a mut self,
        pending_dials: &'a mut FuturesUnordered<BoxFuture<'static, PeerId>>,
    ) {
        // Cancel dials to peers that are no longer eligible.
        self.cancel_stale_dials().await;
        // Disconnect from connected peers that are no longer eligible.
        self.close_stale_connections().await;
        // Dial peers which are eligible but are neither connected nor queued for dialing in the
        // future.
        self.dial_eligible_peers(pending_dials).await;
    }

    fn handle_request(&mut self, req: ConnectivityRequest) {
        match req {
            ConnectivityRequest::UpdateAddresses(peer_id, addrs) => {
                self.peer_addresses.insert(peer_id, addrs);
            }
            ConnectivityRequest::UpdateEligibleNodes(nodes) => {
                *self.eligible.write().unwrap() = nodes;
            }
            ConnectivityRequest::GetDialQueueSize(sender) => {
                sender.send(self.dial_queue.len()).unwrap();
            }
        }
    }

    fn handle_peer_mgr_notification(&mut self, notif: PeerManagerNotification<TSubstream>) {
        match notif {
            PeerManagerNotification::NewPeer(peer_id, addr) => {
                self.connected.insert(peer_id, addr);
                // Remove status of backoff strategy for the connected peer.
                self.dial_backoffs.remove(&peer_id);
                // Cancel possible queued dial to this peer.
                self.dial_queue.remove(&peer_id);
            }
            PeerManagerNotification::LostPeer(peer_id, addr) => {
                match self.connected.get(&peer_id) {
                    Some(curr_addr) if *curr_addr == addr => {
                        // Remove node from connected peers list.
                        self.connected.remove(&peer_id);
                    }
                    _ => {
                        debug!(
                            "Ignoring stale lost peer event for peer: {}, addr: {}",
                            peer_id.short_str(),
                            addr
                        );
                    }
                }
            }
            _ => {
                panic!("Received unexpected notification from peer manager");
            }
        }
    }
}

fn log_dial_result(peer_id: PeerId, addr: Multiaddr, dial_result: DialResult) {
    match dial_result {
        DialResult::Success => {
            info!(
                "Successfully connected to peer: {} at address: {}",
                peer_id.short_str(),
                addr
            );
        }
        DialResult::Cancelled => {
            info!("Cancelled pending dial to peer: {}", peer_id.short_str());
        }
        DialResult::Failed(err) => match err {
            PeerManagerError::AlreadyConnected(a) => {
                info!(
                    "Already connected to peer: {} at address: {}",
                    peer_id.short_str(),
                    a
                );
            }
            e => {
                info!(
                    "Failed to connect to peer: {} at address: {}. Error: {:?}",
                    peer_id.short_str(),
                    addr,
                    e
                );
            }
        },
    }
}
