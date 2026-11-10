//! 생성안/편집안 문서의 모델과 승인 로직을 담는 크레이트.

mod apply;
mod model;
mod store;
mod text;

pub use model::{Change, EditRegion, Proposal, Review};
pub use store::{Edited, Entry, InvalidEntry, Listing, ReviewError, Store};
pub use text::{Config, Encoding, LineEnding};
