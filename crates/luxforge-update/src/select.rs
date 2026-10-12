//! Choosing the update a feed offers.
use crate::{AppcastItem, Channel};

/// The newest item, by build number, on a followed channel whose build is higher than
/// `current_build`, if any. An item on a channel this crate does not know is never eligible. Of
/// two items with the same build, the first in the feed is chosen.
pub fn newest_eligible<'a>(
    items: &'a [AppcastItem],
    current_build: u64,
    followed: &[Channel],
) -> Option<&'a AppcastItem> {
    items
        .iter()
        .filter(|item| item.build > current_build)
        .filter(|item| {
            followed
                .iter()
                .any(|channel| channel.sparkle_name() == item.channel.as_deref())
        })
        .fold(None, |best: Option<&AppcastItem>, item| match best {
            Some(best) if best.build >= item.build => Some(best),
            _ => Some(item),
        })
}
