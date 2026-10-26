//! Which session holds a post or a comment, and the next comment seq. Derived
//! from the loaded session files; rebuilt whenever the folder is reloaded.

use std::collections::HashMap;

use notes_proto::posts::{CommentId, PostId, SessionId};

use crate::json_store::SessionFile;

#[derive(Default)]
pub(crate) struct StageIndex {
    posts: HashMap<PostId, SessionId>,
    comments: HashMap<CommentId, SessionId>,
    /// Creation order of posts, the tie-break for equal `updated_at`.
    post_ord: HashMap<PostId, u64>,
    next_post_ord: u64,
    /// Highest comment seq ever loaded or assigned; seqs are global, like SQLite's rowid.
    max_comment_seq: u64,
}

impl StageIndex {
    pub fn add_session(&mut self, s: &SessionFile) {
        for p in &s.posts {
            self.posts.insert(p.id.clone(), s.id.clone());
            if !self.post_ord.contains_key(&p.id) {
                self.next_post_ord += 1;
                self.post_ord.insert(p.id.clone(), self.next_post_ord);
            }
        }
        for c in &s.comments {
            self.comments.insert(c.id.clone(), s.id.clone());
            self.max_comment_seq = self.max_comment_seq.max(c.seq);
        }
    }

    pub fn remove_session(&mut self, s: &SessionFile) {
        for p in &s.posts {
            self.posts.remove(&p.id);
        }
        for c in &s.comments {
            self.comments.remove(&c.id);
        }
    }

    pub fn post_session(&self, post: &str) -> Option<&SessionId> {
        self.posts.get(post)
    }

    pub fn comment_session(&self, comment: &str) -> Option<&SessionId> {
        self.comments.get(comment)
    }

    pub fn post_ord(&self, post: &str) -> u64 {
        self.post_ord.get(post).copied().unwrap_or(0)
    }

    pub fn next_comment_seq(&self) -> u64 {
        self.max_comment_seq + 1
    }
}
