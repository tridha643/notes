--------------------------- MODULE StageFeedback ---------------------------
(* One stage session's feedback cursor (`agent_seq`, crates/posts/src/store.rs).
   The user comments on cards from stage (comment_new). Coding agents post cards
   (post_new / write_version, which take the new user comments as `feedback`) and
   block for one (`notes wait`, next_user_comment). notesd runs every verb as one
   unit under one lock (Daemon::unit), writes the session file inside the unit
   and replies after it. ../SKILL.md maps every action and variable to the code. *)
EXTENDS Naturals, FiniteSets

CONSTANTS
    MaxComments,  \* comments ever created in the session (bound)
    MaxWaits,     \* `notes wait` clients that can be in flight at once (bound)
    WaitPost,     \* clients may run `notes wait --post P`
    Abandon,      \* a client may go away while its wait blocks (nothing watches the socket)
    ReplyLost,    \* a reply may fail to land after the unit wrote the cursor
    Deletes       \* post_delete / comment_delete remove comments

Posts == {"P1", "P2"}
Targets == Posts \cup {"session"}             \* what a comment is on
Filters == IF WaitPost THEN Posts \cup {"any"} ELSE {"any"}
Waiters == 1..MaxWaits
Ids == 1..MaxComments

Max(S) == CHOOSE s \in S : \A t \in S : t <= s
MinSeq(S) == CHOOSE c \in S : \A d \in S : c.seq <= d.seq

VARIABLES
    comments,   \* SessionFile.comments: [n (identity), seq, user (author = "user"), post]
    created,    \* comments ever created; the next one's identity is created + 1
    maxSeq,     \* StageIndex.max_comment_seq: next seq is maxSeq + 1
    cursor,     \* SessionFile.agent_seq, as written to the session file
    delivered,  \* delivered[n]: replies that reached a client carrying comment n (capped at 2)
    waits,      \* waits[w]: a `notes wait` client: idle, blocked, or gone (left while blocked)
    units       \* units a thread has computed but not yet committed (the lock allows one)

vars == << comments, created, maxSeq, cursor, delivered, waits, units >>

Idle == [state |-> "idle", post |-> "any"]

Init ==
    /\ comments = {}
    /\ created = 0
    /\ maxSeq = 0
    /\ cursor = 0
    /\ delivered = [n \in Ids |-> 0]
    /\ waits = [w \in Waiters |-> Idle]
    /\ units = {}

(* Daemon::unit holds the store lock, so a unit begins only when none is pending. *)
Free == units = {}

(* comment_new: the next global seq, appended to the session. *)
NewComment(user, p) ==
    /\ Free
    /\ created < MaxComments
    /\ created' = created + 1
    /\ maxSeq' = maxSeq + 1
    /\ comments' = comments \cup {[n |-> created + 1, seq |-> maxSeq + 1, user |-> user, post |-> p]}
    /\ UNCHANGED << cursor, delivered, waits, units >>

UserComment == \E p \in Targets : NewComment(TRUE, p)
AgentComment == \E p \in Targets : NewComment(FALSE, p)

(* post_new / write_version: collect_feedback takes every comment past the cursor,
   moves the cursor to the newest of them (agent comments too), and hands the
   user's ones back as `feedback`. *)
BeginPost ==
    /\ Free
    /\ LET fresh == {c \in comments : c.seq > cursor}
       IN units' = units \cup {[kind |-> "post", w |-> 0,
                     hand |-> {c.n : c \in {f \in fresh : f.user}},
                     next |-> IF fresh = {} THEN cursor ELSE Max({c.seq : c \in fresh})]}
    /\ UNCHANGED << comments, created, maxSeq, cursor, delivered, waits >>

(* A client sends `notes wait` (optionally `--post`). *)
StartWait(w) ==
    /\ waits[w].state = "idle"
    /\ \E f \in Filters : waits' = [waits EXCEPT ![w] = [state |-> "blocked", post |-> f]]
    /\ UNCHANGED << comments, created, maxSeq, cursor, delivered, units >>

(* next_user_comment, run by CommentSignal::wait_for under the lock: the oldest
   user comment past the cursor (on the post, when one was given) becomes the
   answer and the cursor moves to it. *)
BeginWait(w) ==
    /\ Free
    /\ waits[w].state \in {"blocked", "gone"}
    /\ LET f == waits[w].post
           cands == {c \in comments : c.user /\ c.seq > cursor /\ (f = "any" \/ c.post = f)}
       IN /\ cands # {}
          /\ LET found == MinSeq(cands)
             IN units' = units \cup {[kind |-> "wait", w |-> w, hand |-> {found.n}, next |-> found.seq]}
    /\ UNCHANGED << comments, created, maxSeq, cursor, delivered, waits >>

Bump(d, hand) == [n \in Ids |-> IF n \in hand /\ d[n] < 2 THEN d[n] + 1 ELSE d[n]]

(* Store::save writes the file with the new cursor, Daemon::unit commits and
   releases the lock, then serve() writes the reply to the socket. The reply
   lands unless the client is gone (nobody reads it) or it is lost on the way. *)
Commit ==
    \E u \in units :
        /\ units' = units \ {u}
        /\ cursor' = u.next
        /\ LET listening == IF u.kind = "post" THEN TRUE ELSE waits[u.w].state = "blocked"
           IN \/ listening /\ delivered' = Bump(delivered, u.hand)
              \/ (ReplyLost \/ ~listening) /\ delivered' = delivered
        /\ waits' = IF u.kind = "wait" THEN [waits EXCEPT ![u.w] = Idle] ELSE waits
        /\ UNCHANGED << comments, created, maxSeq >>

(* wait_for returns None at the timeout (`--timeout`, 120 s by default): nothing consumed. *)
WaitTimeout(w) ==
    /\ waits[w].state \in {"blocked", "gone"}
    /\ ~\E u \in units : u.w = w
    /\ waits' = [waits EXCEPT ![w] = Idle]
    /\ UNCHANGED << comments, created, maxSeq, cursor, delivered, units >>

(* The client dies or is killed (a harness timeout, ctrl-c) while blocked. call()'s
   Wait arm has no reader on the socket, so the daemon thread keeps waiting. *)
Leave(w) ==
    /\ Abandon
    /\ waits[w].state = "blocked"
    /\ ~\E u \in units : u.w = w
    /\ waits' = [waits EXCEPT ![w].state = "gone"]
    /\ UNCHANGED << comments, created, maxSeq, cursor, delivered, units >>

(* comment_delete, or post_delete dropping the post's comments. *)
DeleteComment ==
    /\ Deletes
    /\ Free
    /\ \E c \in comments : comments' = comments \ {c}
    /\ UNCHANGED << created, maxSeq, cursor, delivered, waits, units >>

(* notesd restarts: Stage::load rebuilds max_comment_seq from the comments still
   on disk, agent_seq comes back from the file, a unit that had not committed is
   rolled back, and every connection (blocked waits included) is dropped. *)
Restart ==
    /\ maxSeq' = IF comments = {} THEN 0 ELSE Max({c.seq : c \in comments})
    /\ units' = {}
    /\ waits' = [w \in Waiters |-> Idle]
    /\ UNCHANGED << comments, created, cursor, delivered >>

Next ==
    \/ UserComment \/ AgentComment \/ BeginPost \/ Commit \/ DeleteComment \/ Restart
    \/ \E w \in Waiters : StartWait(w) \/ BeginWait(w) \/ WaitTimeout(w) \/ Leave(w)

Spec == Init /\ [][Next]_vars

(* No user comment reaches the agent twice (feedback lines and wait answers together). *)
Once == \A n \in Ids : delivered[n] <= 1

(* The cursor never passes a user comment the agent has not received: everything
   at or below agent_seq was delivered, everything above is still deliverable. *)
NoLoss == \A c \in comments : c.user /\ c.seq <= cursor => delivered[c.n] >= 1

=============================================================================
