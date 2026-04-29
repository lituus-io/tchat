pub mod googlechat;

use crossbeam::channel::Sender;

use crate::event::OutboundCommand;
use crate::types::PlatformId;

/// Dispatch an outbound command to the correct platform's channel.
///
/// Iterates over the platform channels and sends to the one matching
/// `target`. No dynamic dispatch — just a linear scan over a small Vec.
pub fn dispatch_command(
    channels: &[(PlatformId, Sender<OutboundCommand>)],
    target: PlatformId,
    cmd: OutboundCommand,
) {
    for (id, tx) in channels {
        if *id == target {
            let _ = tx.send(cmd);
            return;
        }
    }
    tracing::warn!("No channel found for platform {:?}", target);
}
