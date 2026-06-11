//! In-memory fan-out registry: user → live connections, room (stream) → users.
//! O(connections) memory; idle sockets are bounded because mobile clients close on
//! background (manifesto #5).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use tokio::sync::RwLock;

#[derive(Default)]
pub struct Hub {
    clients: RwLock<HashMap<String, Vec<(u64, mpsc::UnboundedSender<String>)>>>,
    rooms: RwLock<HashMap<String, HashSet<String>>>,
    next_conn: AtomicU64,
}

impl Hub {
    pub async fn register(&self, user: &str) -> (u64, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = self.next_conn.fetch_add(1, Ordering::Relaxed);
        self.clients
            .write()
            .await
            .entry(user.to_string())
            .or_default()
            .push((id, tx));
        (id, rx)
    }

    pub async fn unregister(&self, user: &str, conn_id: u64) {
        let mut clients = self.clients.write().await;
        let mut gone = false;
        if let Some(conns) = clients.get_mut(user) {
            conns.retain(|(id, _)| *id != conn_id);
            if conns.is_empty() {
                clients.remove(user);
                gone = true;
            }
        }
        drop(clients);
        if gone {
            let mut rooms = self.rooms.write().await;
            for members in rooms.values_mut() {
                members.remove(user);
            }
            rooms.retain(|_, m| !m.is_empty());
        }
    }

    /// Send to every live connection of a user. Returns false if none delivered
    /// (caller falls back to a push notification).
    pub async fn send_to_user(&self, user: &str, msg: &str) -> bool {
        let clients = self.clients.read().await;
        let Some(conns) = clients.get(user) else {
            return false;
        };
        let mut delivered = false;
        for (_, tx) in conns {
            delivered |= tx.send(msg.to_string()).is_ok();
        }
        delivered
    }

    pub async fn join_room(&self, room: &str, user: &str) {
        self.rooms
            .write()
            .await
            .entry(room.to_string())
            .or_default()
            .insert(user.to_string());
    }

    pub async fn leave_room(&self, room: &str, user: &str) {
        let mut rooms = self.rooms.write().await;
        if let Some(members) = rooms.get_mut(room) {
            members.remove(user);
            if members.is_empty() {
                rooms.remove(room);
            }
        }
    }

    pub async fn broadcast_room(&self, room: &str, msg: &str, except: Option<&str>) {
        let members: Vec<String> = {
            let rooms = self.rooms.read().await;
            match rooms.get(room) {
                Some(m) => m.iter().cloned().collect(),
                None => return,
            }
        };
        for user in members {
            if Some(user.as_str()) != except {
                self.send_to_user(&user, msg).await;
            }
        }
    }

    pub async fn close_room(&self, room: &str) {
        self.rooms.write().await.remove(room);
    }

    pub async fn online(&self, user: &str) -> bool {
        self.clients.read().await.contains_key(user)
    }
}
