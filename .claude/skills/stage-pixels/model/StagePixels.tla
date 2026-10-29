---------------------------- MODULE StagePixels ----------------------------
(* One Mermaid card in stage: in the feed (with or without the sidebar) and
   zoomed in focus (z), while rasters land in any order, a zoom is made ahead
   for the card under the cursor, and the feed pixels are parked while zoomed.
   ../SKILL.md maps every action and variable to the code it stands for. *)
EXTENDS Naturals, FiniteSets

CONSTANTS MaxIds, MaxJobs

None == "none"
(* What a raster is made for (gallery.rs RasterTarget): the feed with the
   sidebar shown, the feed without it, or zoomed into the focused pane. Each
   has its own pixel size, so an image made for one is resampled in another. *)
Made == {"N", "W", "Z"}
NoImg == [id |-> 0, made |-> None, previous |-> 0, placed |-> FALSE]
NoZoom == [on |-> FALSE, madeFor |-> None, img |-> NoImg]
(* Gallery::park: the feed image as set aside, without pixels held for a resize. *)
Parked(i) == IF i.previous # 0 THEN [i EXCEPT !.previous = 0, !.placed = FALSE] ELSE i

VARIABLES
    \* App (app.rs)
    appFocused,     \* focused_card is Some
    sidebarHidden,  \* sidebar_hidden
    visuals,        \* visuals[key]: the size (by what it was made for) the layout uses; None = Pending
    unzoomed,       \* unzoomed: the feed size saved when focus opened
    zoomAhead,      \* zoom_ahead: "none" | "asked" | "ready" (ZoomReady arrived)
    leftFocus,      \* left_focus: what the card showed when focus closed, until the next draw
    \* Gallery (gallery.rs)
    gFocused,       \* focused == Some(key), as of the last retarget
    gWidth,         \* target width as of the last retarget: "N" | "W" | "F" (focus)
    rastered,       \* rastered[key]: what the latest raster job was started for
    jobs,           \* raster jobs in flight, by what they were made for
    img,            \* images[key]: id, made, previous (held until placed), placed
    zoom,           \* zoom: Unzoomed { made_for, img }
    prefetch,       \* prefetch: the zoom made ahead (made for "Z"), done or not
    readyZoom,      \* ready_zoom: prefetched pixels to show at the next place
    dead,           \* dead: ids to delete at the next place
    \* Terminal (kitty)
    shown,          \* ids with a placement on screen for this card
    held,           \* ids whose pixels the terminal stores
    madeOf,         \* what each id's pixels were made for
    nextId,         \* next_id
    \* main.rs loop
    dirty           \* a redraw is due

app == << appFocused, sidebarHidden, visuals, unzoomed, zoomAhead, leftFocus >>
gallery == << gFocused, gWidth, rastered, jobs, img, zoom, prefetch, readyZoom, dead >>
term == << shown, held, madeOf, nextId >>
vars == << app, gallery, term, dirty >>

(* App::target() as the gallery's width, and what the card should be made for. *)
AppWidth == IF appFocused THEN "F" ELSE IF sidebarHidden THEN "W" ELSE "N"
Wanted == IF appFocused THEN "Z" ELSE IF sidebarHidden THEN "W" ELSE "N"
(* Gallery::raster_target for this card. *)
RT(f, w) == IF f THEN "Z" ELSE w

Init ==
    /\ appFocused = FALSE /\ sidebarHidden = FALSE /\ visuals = None
    /\ unzoomed = None /\ zoomAhead = "none" /\ leftFocus = None
    \* The helper's SVG arrived and its first raster started (handle: Pending::Mermaid).
    /\ gFocused = FALSE /\ gWidth = "N" /\ rastered = "N"
    /\ jobs = [m \in Made |-> IF m = "N" THEN 1 ELSE 0]
    /\ img = NoImg /\ zoom = NoZoom /\ prefetch = [on |-> FALSE, done |-> FALSE]
    /\ readyZoom = FALSE /\ dead = {}
    /\ shown = {} /\ held = {} /\ madeOf = [i \in 1..MaxIds |-> None] /\ nextId = 1
    /\ dirty = TRUE

(* Gallery::show for raster pixels made for m, given the card's current image
   cur; returns the new image, the ids freed, and whether a new id was taken. *)
ShowNewId(cur, m) ==
    \/ cur.id = 0
    \/ (cur.made # m /\ (cur.placed \/ cur.previous # 0))
Shown(cur, m) ==
    LET new == ShowNewId(cur, m)
        id == IF new THEN nextId ELSE cur.id
        \* resized while placed: the old id is held as previous (or, when one is
        \* already held, the current id is deleted and previous stays)
        prev == IF new /\ cur.id # 0 THEN (IF cur.previous # 0 THEN cur.previous ELSE cur.id) ELSE cur.previous
        keepPlacement == ~new /\ cur.made = m /\ cur.placed /\ cur.previous = 0
        placed == IF new THEN cur.placed ELSE (keepPlacement \/ (cur.previous # 0 /\ cur.placed))
    IN [id |-> id, made |-> m, previous |-> prev, placed |-> placed]
ShowFreed(cur, m) ==
    IF ShowNewId(cur, m) /\ cur.id # 0 /\ cur.previous # 0 THEN {cur.id} ELSE {}

(* A raster job finishes (Gfx::Raster in Gallery::handle, then the app's
   VisualReady or ZoomReady in the same loop turn, before the next draw). *)
RasterDone(m) ==
    /\ jobs[m] > 0
    /\ jobs' = [jobs EXCEPT ![m] = @ - 1]
    /\ IF ~gFocused /\ prefetch.on /\ ~prefetch.done /\ m = "Z"
       THEN \* the zoom made ahead: held until focus opens on the card
            /\ prefetch' = [prefetch EXCEPT !.done = TRUE]
            /\ zoomAhead' = IF zoomAhead = "asked" THEN "ready" ELSE zoomAhead
            /\ UNCHANGED << appFocused, sidebarHidden, visuals, unzoomed, leftFocus,
                            gFocused, gWidth, rastered, img, zoom, readyZoom, dead, term, dirty >>
       ELSE IF m # RT(gFocused, gWidth)
       THEN \* made for another target: dropped, and asked for again when on screen
            /\ rastered' = IF rastered = m THEN None ELSE rastered
            /\ zoom' = IF zoom.on /\ zoom.madeFor = m THEN [zoom EXCEPT !.madeFor = None] ELSE zoom
            /\ UNCHANGED << app, gFocused, gWidth, img, prefetch, readyZoom, dead, term, dirty >>
       ELSE \* shown; a zoomed one parks the feed image first
            LET park == m = "Z" /\ zoom.on /\ zoom.img.id = 0
                cur == IF park THEN NoImg ELSE img
                new == Shown(cur, m)
                ready == cur.id = 0 \/ cur.made # m
            IN /\ zoom' = IF park THEN [zoom EXCEPT !.img = Parked(img)] ELSE zoom
               /\ dead' = IF park /\ img.previous # 0 THEN dead \cup {img.previous} ELSE dead
               /\ img' = new
               /\ held' = (held \ ShowFreed(cur, m)) \cup {new.id}
               /\ shown' = shown \ ShowFreed(cur, m)
               /\ madeOf' = [madeOf EXCEPT ![new.id] = m]
               /\ nextId' = IF ShowNewId(cur, m) THEN nextId + 1 ELSE nextId
               \* VisualReady (fresh or resized): the app lays the card out at m's size;
               \* ZoomShown for a zoomed one, which the app ignores once out of focus
               /\ visuals' = IF ready /\ (m # "Z" \/ appFocused) THEN m ELSE visuals
               \* feed pixels landing while zoomed are what leaving focus restores
               /\ unzoomed' = IF ready /\ m # "Z" /\ unzoomed # None THEN m ELSE unzoomed
               \* a zoom shown after focus closed, before the gallery drew it closed
               /\ leftFocus' = IF ready /\ m = "Z" /\ ~appFocused /\ leftFocus # None THEN "Z" ELSE leftFocus
               /\ dirty' = (dirty \/ ready)
               /\ UNCHANGED << appFocused, sidebarHidden, zoomAhead,
                               gFocused, gWidth, rastered, prefetch, readyZoom >>

(* z (App::toggle_card_focus / enter_card_focus / leave_focus). *)
PressZ ==
    /\ visuals # None
    /\ IF ~appFocused
       THEN /\ appFocused' = TRUE
            /\ unzoomed' = visuals
            \* back before a draw: as focus showed it; else a zoom made ahead
            /\ visuals' = IF leftFocus # None THEN leftFocus
                          ELSE IF zoomAhead = "ready" THEN "Z" ELSE visuals
            /\ leftFocus' = None
       ELSE /\ appFocused' = FALSE
            /\ visuals' = IF unzoomed # None THEN unzoomed ELSE visuals
            /\ leftFocus' = IF unzoomed # None THEN visuals ELSE None
            /\ unzoomed' = None
    /\ zoomAhead' = "none"
    /\ dirty' = TRUE
    /\ UNCHANGED << sidebarHidden, gallery, term >>

(* s with no HTML card waiting (App::toggle_sidebar: immediate). *)
PressS ==
    /\ ~appFocused
    /\ sidebarHidden' = ~sidebarHidden
    /\ dirty' = TRUE
    /\ UNCHANGED << appFocused, visuals, unzoomed, zoomAhead, leftFocus, gallery, term >>

(* One draw in main.rs: ui::draw lays out from the app, then Gallery::retarget,
   Gallery::place, and App::after_draw (zoom_ahead -> Gallery::prefetch_zoom). *)
Draw ==
    /\ dirty
    /\ LET f == appFocused
           w == AppWidth
           changed == f # gFocused \/ w # gWidth
           \* retarget: focus opening on the card takes the zoom made ahead
           takeP == changed /\ f /\ prefetch.on
           z1 == IF takeP THEN [on |-> TRUE, madeFor |-> rastered, img |-> NoImg] ELSE zoom
           r1 == IF takeP THEN "Z" ELSE rastered
           ready1 == IF takeP THEN prefetch.done ELSE readyZoom
           \* retarget: focus leaving puts the parked feed image back
           leave == changed /\ z1.on /\ ~f
           unpark == leave /\ z1.img.id # 0
           img1 == IF unpark THEN z1.img ELSE img
           dead1 == dead \cup (IF unpark /\ img.id # 0 THEN {img.id, img.previous} \ {0} ELSE {})
           r2 == IF leave THEN z1.madeFor ELSE r1
           z2 == IF leave THEN NoZoom ELSE z1
           \* place: prefetched pixels first, if the app laid the card out at their
           \* size; else they go back through the loop as a finished raster
           now == ready1 /\ visuals = "Z"
           resend == ready1 /\ ~now
           parkR == now /\ z2.on /\ z2.img.id = 0
           curR == IF parkR THEN NoImg ELSE img1
           img2 == IF now THEN Shown(curR, "Z") ELSE img1
           z3 == IF parkR THEN [z2 EXCEPT !.img = Parked(img1)] ELSE z2
           dead2 == IF parkR /\ img1.previous # 0 THEN dead1 \cup {img1.previous} ELSE dead1
           freedR == IF now THEN ShowFreed(curR, "Z") ELSE {}
           heldR == IF now THEN (held \ freedR) \cup {img2.id} ELSE held
           madeR == IF now THEN [madeOf EXCEPT ![img2.id] = "Z"] ELSE madeOf
           idR == IF now /\ ShowNewId(curR, "Z") THEN nextId + 1 ELSE nextId
           \* place: on-screen diagrams rasterize for the current target
           onScreen == visuals # None
           tgt == RT(f, IF f THEN gWidth ELSE w)
           go == onScreen /\ r2 # tgt
           z4 == IF go /\ tgt = "Z" /\ ~z3.on THEN [on |-> TRUE, madeFor |-> r2, img |-> NoImg] ELSE z3
           r3 == IF go THEN tgt ELSE r2
           jobs0 == IF resend THEN [jobs EXCEPT !["Z"] = @ + 1] ELSE jobs
           jobs1 == IF go THEN [jobs0 EXCEPT ![tgt] = @ + 1] ELSE jobs0
           \* place: the card's image, holding a resized one until the layout uses its size
           hold == img2.id # 0 /\ img2.previous # 0 /\ img2.placed /\ onScreen /\ visuals # img2.made
           dropPrev == img2.id # 0 /\ ~hold /\ img2.previous # 0
           img3 == IF img2.id = 0 \/ hold THEN img2
                   ELSE [img2 EXCEPT !.previous = 0, !.placed = onScreen]
           shown1 == IF img2.id = 0 \/ hold THEN shown \ freedR
                     ELSE ((shown \ freedR) \ {img2.previous}) \cup (IF onScreen THEN {img2.id} ELSE {})
           shown2 == IF onScreen THEN shown1 ELSE shown1 \ {img2.id}
           held1 == IF dropPrev THEN heldR \ {img2.previous} ELSE heldR
           \* place: the parked feed image is hidden (kept), then the dead are freed
           z5 == IF z4.img.id # 0 THEN [z4 EXCEPT !.img.placed = FALSE] ELSE z4
           shown3 == (IF z4.img.id # 0 THEN shown2 \ {z4.img.id} ELSE shown2) \ dead2
           held2 == held1 \ dead2
           \* after_draw: the card under the cursor gets its zoom made ahead
           ask == ~f /\ onScreen /\ zoomAhead = "none"
           startP == ask /\ ~f /\ ~prefetch.on
       IN /\ gFocused' = f /\ gWidth' = IF f THEN "F" ELSE w
          /\ rastered' = r3
          /\ jobs' = IF startP THEN [jobs1 EXCEPT !["Z"] = @ + 1] ELSE jobs1
          /\ img' = img3
          /\ zoom' = z5
          /\ prefetch' = IF startP THEN [on |-> TRUE, done |-> FALSE]
                         ELSE IF takeP THEN [on |-> FALSE, done |-> FALSE] ELSE prefetch
          /\ readyZoom' = FALSE
          /\ dead' = {}
          /\ shown' = shown3 /\ held' = held2 /\ madeOf' = madeR /\ nextId' = idR
          \* Gallery::prefetch_zoom says ZoomReady again for a zoom already made
          /\ zoomAhead' = IF ask THEN (IF prefetch.on /\ prefetch.done /\ ~takeP THEN "ready" ELSE "asked") ELSE zoomAhead
          /\ leftFocus' = None
          /\ dirty' = FALSE
          /\ UNCHANGED << appFocused, sidebarHidden, visuals, unzoomed >>

Next ==
    \/ Draw
    \/ PressZ
    \/ PressS
    \/ \E m \in Made : RasterDone(m)

Spec == Init /\ [][Next]_vars

(* Bounds: image ids and jobs in flight. *)
Bounds == nextId <= MaxIds /\ \A m \in Made : jobs[m] <= MaxJobs

Idle == \A m \in Made : jobs[m] = 0

(* After every draw, each image showing for the card has the pixel size the
   layout gives it: kitty never resamples a diagram, not even for one frame. *)
OneToOne == ~dirty /\ visuals # None => \A i \in shown : madeOf[i] = visuals

(* After every draw, the card shows at most one image. *)
OneImage == ~dirty => Cardinality(shown) <= 1

(* Once every raster has landed and been drawn, the card shows one image made
   for where it is now: zoomed when focused, else for the sidebar state. *)
Crisp == ~dirty /\ Idle /\ visuals # None =>
            /\ shown = {img.id}
            /\ madeOf[img.id] = Wanted
            /\ visuals = Wanted

(* After every draw, the terminal stores only pixels stage can still show. *)
NoLeak == ~dirty => held \subseteq ({img.id, img.previous, zoom.img.id, zoom.img.previous} \ {0})
=============================================================================
