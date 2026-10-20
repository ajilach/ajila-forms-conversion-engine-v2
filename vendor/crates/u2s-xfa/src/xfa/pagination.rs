//! Breaking a laid-out column into content areas, and choosing the page each
//! one is on (XFA 3.3 §7 "Break Conditions", §8 "Content Splitting" and
//! "Pagination Strategies").
//!
//! The flattener lays the Form DOM out as one tall column and records, for
//! every object in it, an [`Extent`]: where it is, whether it may be split,
//! and what it asks of the pagination (`keep`, `breakBefore`, `breakAfter`).
//! [`split_point`] then finds where a content area may end, by the split
//! consensus the specification describes, and [`Paginator`] picks the
//! `pageArea` of every page by the page set's pagination strategy. Both are
//! pure: they know nothing of nodes or rendering.

use std::collections::BTreeSet;

use super::Num;

// ---------------------------------------------------------------------------
// Extents and content splitting
// ---------------------------------------------------------------------------

/// Where a layout object may be split (XFA 3.3 §8 "Split Restrictions").
#[derive(Debug, Clone, PartialEq)]
pub enum Splitting {
    /// Not at all: an image, a geometric figure, a barcode, a widget, a
    /// `keep intact` container, a draw (whose `intact` defaults to
    /// `contentArea`).
    Never,
    /// Only at these column Y positions: a multi-line text's line
    /// boundaries ("below any line, at the position of the lowest
    /// descender"). Empty means the text is one line, which cannot split.
    Between(Vec<Num>),
    /// Anywhere its children allow, except inside its own top and bottom
    /// margins ("Splitting within a top or bottom margin is not allowed").
    Inside { margin_top: Num, margin_bottom: Num },
}

/// What a break request targets (XFA 3.3 §7 "Break Conditions", the
/// `targetType` of `breakBefore`/`breakAfter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakTarget {
    /// The next content area (on the current page when one is left).
    ContentArea,
    /// The next page.
    PageArea,
    /// The next even-numbered page.
    PageEven,
    /// The next odd-numbered page.
    PageOdd,
}

/// A `breakBefore`/`breakAfter` that forces a move to a new content area or
/// page. A `targetType="auto"` (the default) forces nothing and is never
/// recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakRequest {
    pub target_type: BreakTarget,
    /// The `target`: the name of a `pageArea`, `pageSet` or `contentArea`
    /// (a SOM expression's last segment, or `#id`), when one is named.
    pub target: Option<String>,
    /// `startNew="1"`: a new page or content area even when the current
    /// one is still empty.
    pub start_new: bool,
}

/// One laid-out object of the column: its vertical extent, how it may be
/// split, its children, and what it asks of the pagination.
#[derive(Debug, Clone, PartialEq)]
pub struct Extent {
    pub top: Num,
    pub bottom: Num,
    pub splitting: Splitting,
    pub children: Vec<Extent>,
    /// `keep next="contentArea|pageArea"`: it and the next sibling must
    /// land in the same content area (XFA 3.3 §8 "Adhesion").
    pub keep_next: bool,
    /// `keep previous="contentArea|pageArea"`: it and the previous sibling
    /// must land in the same content area.
    pub keep_previous: bool,
    pub break_before: Option<BreakRequest>,
    pub break_after: Option<BreakRequest>,
}

impl Extent {
    /// An extent with nothing asked of the pagination.
    pub fn new(top: Num, bottom: Num, splitting: Splitting) -> Self {
        Extent {
            top,
            bottom,
            splitting,
            children: Vec::new(),
            keep_next: false,
            keep_previous: false,
            break_before: None,
            break_after: None,
        }
    }
}

/// A break the template forces, at `at` in column coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedBreak {
    pub at: Num,
    pub request: BreakRequest,
}

/// Every forced break in `extents`, in column order: a `breakBefore` at its
/// object's top, a `breakAfter` at its bottom. Two at the same position are
/// one break that asks for both: a named target wins over none, a
/// `startNew` over none, and a page over a content area.
pub fn forced_breaks(extents: &[Extent]) -> Vec<ForcedBreak> {
    fn walk(extents: &[Extent], out: &mut Vec<ForcedBreak>) {
        for e in extents {
            if let Some(r) = &e.break_before {
                out.push(ForcedBreak {
                    at: e.top,
                    request: r.clone(),
                });
            }
            walk(&e.children, out);
            if let Some(r) = &e.break_after {
                out.push(ForcedBreak {
                    at: e.bottom,
                    request: r.clone(),
                });
            }
        }
    }
    let mut all = Vec::new();
    walk(extents, &mut all);
    all.sort_by_key(|b| b.at);
    let mut merged: Vec<ForcedBreak> = Vec::new();
    for b in all {
        match merged.last_mut() {
            Some(last) if last.at == b.at => {
                let r = &mut last.request;
                if r.target.is_none() {
                    r.target = b.request.target;
                }
                r.start_new |= b.request.start_new;
                if r.target_type == BreakTarget::ContentArea {
                    r.target_type = b.request.target_type;
                }
            }
            _ => merged.push(b),
        }
    }
    merged
}

/// A span the break may not fall in, and where it goes instead.
struct NoSplitZone {
    /// The break may not fall in `(low, high]`.
    low: Num,
    high: Num,
    /// The lowest permissible location above the zone.
    permissible: Num,
}

/// Adhesion zones (XFA 3.3 §8 "Adhesion"): a `keep next` object may not be
/// parted from its next sibling, so no break may fall inside it or exactly
/// between the two; a `keep previous` object the same with its previous
/// sibling. Only the adhering edge and the first fragment adhere, so a
/// split inside the neighbour itself stays legal.
fn adhesion_zones(extents: &[Extent], out: &mut Vec<NoSplitZone>) {
    for (i, e) in extents.iter().enumerate() {
        if e.keep_next
            && let Some(next) = extents.get(i + 1)
        {
            out.push(NoSplitZone {
                low: e.top,
                high: next.top,
                permissible: e.top,
            });
        }
        if e.keep_previous && i > 0 {
            let prev = &extents[i - 1];
            out.push(NoSplitZone {
                low: prev.top,
                high: e.top,
                permissible: prev.top,
            });
        }
        adhesion_zones(&e.children, out);
    }
}

/// The lowest permissible split location above `at` for an object that
/// cannot split at `at`, or `None` when it can.
fn objection(e: &Extent, at: Num) -> Option<Num> {
    if !(e.top < at && at < e.bottom) {
        return None;
    }
    match &e.splitting {
        Splitting::Never => Some(e.top),
        Splitting::Between(points) => {
            if points.contains(&at) {
                None
            } else {
                Some(
                    points
                        .iter()
                        .copied()
                        .filter(|p| *p < at)
                        .max()
                        .unwrap_or(e.top),
                )
            }
        }
        Splitting::Inside {
            margin_top,
            margin_bottom,
        } => {
            if at < e.top + margin_top {
                Some(e.top)
            } else if at > e.bottom - margin_bottom {
                Some((e.bottom - margin_bottom).max(e.top))
            } else {
                None
            }
        }
    }
}

/// The highest objection to a split at `at` among `extents` and their
/// descendants: the permissible location closest to `at`.
fn highest_objection(extents: &[Extent], at: Num) -> Option<Num> {
    let mut best: Option<Num> = None;
    for e in extents {
        if let Some(p) = objection(e, at) {
            best = Some(best.map_or(p, |b| b.max(p)));
        }
        if e.top < at
            && at < e.bottom
            && let Some(p) = highest_objection(&e.children, at)
        {
            best = Some(best.map_or(p, |b| b.max(p)));
        }
    }
    best
}

/// Where a content area starting at `region_start` may end, given that it
/// would end at `desired` if content were infinitely divisible: the split
/// consensus of XFA 3.3 §8 "Splitting a Container Having Child Containers".
///
/// Starting from `desired`, while any object cannot split at the current
/// location, the location moves up to the lowest permissible one for that
/// object -- the one closest to it when several object -- until every
/// object accepts it. `None` when that walks back to the start of the
/// content area: nothing fits, not even by splitting, and the caller
/// applies "Overriding ContentArea Boundaries".
pub fn split_point(extents: &[Extent], region_start: Num, desired: Num) -> Option<Num> {
    let mut zones = Vec::new();
    adhesion_zones(extents, &mut zones);
    let mut at = desired;
    loop {
        if at <= region_start {
            return None;
        }
        let mut objection = highest_objection(extents, at);
        for z in &zones {
            if z.low < at && at <= z.high {
                objection = Some(objection.map_or(z.permissible, |o| o.max(z.permissible)));
            }
        }
        match objection {
            // A permissible location is always above `at`, so this makes
            // progress; a location at or above the region start ends it.
            Some(p) if p < at => at = p,
            Some(_) => return None,
            None => return Some(at),
        }
    }
}

/// The lowest permissible split location at or below `at` for an object
/// that cannot split at `at`, or `None` when it can: the mirror of
/// [`objection`], used when a content area's boundary is overridden.
fn objection_below(e: &Extent, at: Num) -> Option<Num> {
    if !(e.top < at && at < e.bottom) {
        return None;
    }
    match &e.splitting {
        Splitting::Never => Some(e.bottom),
        Splitting::Between(points) => {
            if points.contains(&at) {
                None
            } else {
                Some(points.iter().copied().filter(|p| *p > at).min().unwrap_or(e.bottom))
            }
        }
        Splitting::Inside { margin_top, margin_bottom } => {
            if at < e.top + margin_top {
                Some((e.top + margin_top).min(e.bottom))
            } else if at > e.bottom - margin_bottom {
                Some(e.bottom)
            } else {
                None
            }
        }
    }
}

fn lowest_objection_below(extents: &[Extent], at: Num) -> Option<Num> {
    let mut best: Option<Num> = None;
    for e in extents {
        if let Some(p) = objection_below(e, at) {
            best = Some(best.map_or(p, |b| b.min(p)));
        }
        if e.top < at && at < e.bottom
            && let Some(p) = lowest_objection_below(&e.children, at)
        {
            best = Some(best.map_or(p, |b| b.min(p)));
        }
    }
    best
}

/// Where a content area ends when nothing of its content fits it, not even
/// split (XFA 3.3 §8 "Overriding ContentArea Boundaries": the layout
/// processor "overrides the boundaries of the current content region to
/// force the layout object down onto the page"): the first location at or
/// below `desired` that every object accepts, so the object the content
/// area could not hold is on it whole, past its boundary, and the next
/// content area starts after it.
pub fn split_point_below(extents: &[Extent], desired: Num) -> Num {
    let mut zones = Vec::new();
    adhesion_zones(extents, &mut zones);
    let mut at = desired;
    loop {
        let mut objection = lowest_objection_below(extents, at);
        for z in &zones {
            if z.low < at && at <= z.high {
                // Past the zone: just below its end.
                let past = z.high;
                objection = Some(objection.map_or(past, |o| o.min(past)));
            }
        }
        match objection {
            Some(p) if p > at => at = p,
            _ => return at,
        }
    }
}

// ---------------------------------------------------------------------------
// Page selection
// ---------------------------------------------------------------------------

/// A page set's pagination strategy (XFA 3.3 §8 "Pagination Strategies",
/// the `relation` of `pageSet`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Relation {
    #[default]
    OrderedOccurrence,
    SimplexPaginated,
    DuplexPaginated,
}

impl Relation {
    /// `relation` as written; anything unrecognised is the default, as the
    /// specification's "Error Handling" asks (reported by the caller).
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value {
            None | Some("orderedOccurrence") => Ok(Relation::OrderedOccurrence),
            Some("simplexPaginated") => Ok(Relation::SimplexPaginated),
            Some("duplexPaginated") => Ok(Relation::DuplexPaginated),
            Some(other) => Err(format!("unknown pageSet relation {other:?}")),
        }
    }
}

/// `pagePosition` of a `pageArea`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PagePosition {
    First,
    Last,
    Rest,
    Only,
    #[default]
    Any,
}

impl PagePosition {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value {
            None | Some("any") => Ok(PagePosition::Any),
            Some("first") => Ok(PagePosition::First),
            Some("last") => Ok(PagePosition::Last),
            Some("rest") => Ok(PagePosition::Rest),
            Some("only") => Ok(PagePosition::Only),
            Some(other) => Err(format!("unknown pagePosition {other:?}")),
        }
    }

    /// `first`, `last` and `only` are used at most once per page set.
    fn once(self) -> bool {
        matches!(
            self,
            PagePosition::First | PagePosition::Last | PagePosition::Only
        )
    }
}

/// `oddOrEven` of a `pageArea`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OddOrEven {
    Odd,
    Even,
    #[default]
    Any,
}

impl OddOrEven {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value {
            None | Some("any") => Ok(OddOrEven::Any),
            Some("odd") => Ok(OddOrEven::Odd),
            Some("even") => Ok(OddOrEven::Even),
            Some(other) => Err(format!("unknown oddOrEven {other:?}")),
        }
    }
}

/// `blankOrNotBlank` of a `pageArea`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlankOrNotBlank {
    Blank,
    NotBlank,
    #[default]
    Any,
}

impl BlankOrNotBlank {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value {
            None | Some("any") => Ok(BlankOrNotBlank::Any),
            Some("blank") => Ok(BlankOrNotBlank::Blank),
            Some("notBlank") => Ok(BlankOrNotBlank::NotBlank),
            Some(other) => Err(format!("unknown blankOrNotBlank {other:?}")),
        }
    }
}

/// What the pagination needs to know of one `pageArea`.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaSpec {
    pub name: Option<String>,
    pub position: PagePosition,
    pub odd_or_even: OddOrEven,
    pub blank: BlankOrNotBlank,
    /// `occur min`, for ordered pagination (default 1).
    pub min_occur: u32,
    /// `occur max`, for ordered pagination; `None` is unlimited (the
    /// default for a page).
    pub max_occur: Option<u32>,
    /// How many `contentArea`s it has. None makes it pure boilerplate.
    pub content_areas: usize,
}

/// What the pagination needs to know of one `pageSet`.
#[derive(Debug, Clone, PartialEq)]
pub struct SetSpec {
    pub name: Option<String>,
    pub relation: Relation,
    /// Its direct `pageArea` children, as indices into [`PageSets::areas`],
    /// in document order.
    pub areas: Vec<usize>,
    /// Its direct `pageSet` children, as indices into [`PageSets::sets`],
    /// in document order.
    pub children: Vec<usize>,
}

/// Every `pageSet` and `pageArea` of a template, in document order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageSets {
    pub areas: Vec<AreaSpec>,
    pub sets: Vec<SetSpec>,
}

impl PageSets {
    /// The set an area is a direct child of.
    pub fn set_of_area(&self, area: usize) -> usize {
        self.sets
            .iter()
            .position(|s| s.areas.contains(&area))
            .expect("every area is a child of a set")
    }

    /// The set actually used when `set` is entered: a set with no areas of
    /// its own but child sets stands for its first child (nested page sets,
    /// XFA 3.3 §8 "Nested page sets"). A set with neither has no pages.
    pub fn effective_set(&self, mut set: usize) -> Option<usize> {
        loop {
            let s = &self.sets[set];
            if !s.areas.is_empty() {
                return Some(set);
            }
            set = *s.children.first()?;
        }
    }

    /// What a break `target` names: a `pageArea`, or a `pageSet` (whose
    /// first qualified area is then used).
    pub fn resolve_target(&self, target: &str) -> Option<Target> {
        let last = target.rsplit('.').next().unwrap_or(target);
        if let Some(i) = self
            .areas
            .iter()
            .position(|a| a.name.as_deref() == Some(last))
        {
            return Some(Target::Area(i));
        }
        self.sets
            .iter()
            .position(|s| s.name.as_deref() == Some(last))
            .map(Target::Set)
    }
}

/// What a break target resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Area(usize),
    Set(usize),
}

/// One page the pagination decided on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPage {
    /// Index into [`PageSets::areas`].
    pub area: usize,
    /// A page with no foreground content: a filler for a break to an odd
    /// or even page, a blank designated last page, or an occurrence
    /// minimum being met.
    pub blank: bool,
}

/// Why the pagination is asked for a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageRequest {
    /// The content overflowed, or a break names no page.
    Next,
    /// A break to a named `pageArea` or `pageSet`.
    Target(Target),
    /// A break to the next odd or even page.
    Parity(Parity),
}

/// Odd (front) or even (back) surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parity {
    Odd,
    Even,
}

/// What the pagination could not do as specified. The caller reports it
/// and applies the documented fallback the variant names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaginationError {
    /// No `pageArea` of the current set matches the layout state (XFA 3.3
    /// §8: a fatal error). Fallback: the set's first area.
    NoQualifiedArea { set: Option<String> },
    /// Every `pageArea` of the current ordered set has reached its
    /// `occur max`. Fallback: the set's last area, once more.
    Exhausted { set: Option<String> },
    /// The template has no `pageArea` at all.
    NoPages,
}

/// What termination processing did to the current page set's last page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    /// The last page was laid out again on the designated last (or only)
    /// page, and the content fit.
    Replaced,
    /// The content did not fit the designated last page; a blank one was
    /// added after the last page.
    BlankAppended,
    /// The set designates no last page, or the current page is itself a
    /// first or last page.
    Unchanged,
}

/// Chooses the `pageArea` of every page, in order, by the pagination
/// strategy of the page set the layout is in (XFA 3.3 §8 "Pagination
/// Strategies"). The caller asks for a page whenever it needs one and
/// reports, when the content ends or leaves the page set, whether the tail
/// fits a designated last page.
#[derive(Debug, Clone)]
pub struct Paginator<'a> {
    specs: &'a PageSets,
    pages: Vec<PlannedPage>,
    /// The set the current contiguous sequence of pages comes from.
    current_set: usize,
    /// Index into `pages` of that sequence's first page.
    run_start: usize,
    /// `first`/`last`/`only` areas already used in the current sequence.
    used_once: BTreeSet<usize>,
    /// Per area, how often it was used (ordered pagination).
    occurrences: Vec<u32>,
}

impl<'a> Paginator<'a> {
    /// A paginator positioned before its first page, entered at `entry`
    /// (XFA 3.3 §8 "Determining the start point": the root subform's or its
    /// first subform child's `breakBefore` target) or, without one, at the
    /// first page set in document order.
    pub fn new(specs: &'a PageSets, entry: Option<Target>) -> Result<Self, PaginationError> {
        if specs.areas.is_empty() {
            return Err(PaginationError::NoPages);
        }
        let set = match entry {
            Some(Target::Area(a)) => specs.set_of_area(a),
            Some(Target::Set(s)) => s,
            None => 0,
        };
        let current_set = specs.effective_set(set).ok_or(PaginationError::NoPages)?;
        Ok(Paginator {
            specs,
            pages: Vec::new(),
            current_set,
            run_start: 0,
            used_once: BTreeSet::new(),
            occurrences: vec![0; specs.areas.len()],
        })
    }

    pub fn pages(&self) -> &[PlannedPage] {
        &self.pages
    }

    pub fn into_pages(self) -> Vec<PlannedPage> {
        self.pages
    }

    pub fn current_set(&self) -> usize {
        self.current_set
    }

    /// The set a request would move the layout into, when it is not the
    /// current one. The caller finishes the current set first.
    pub fn set_change(&self, request: PageRequest) -> Option<usize> {
        let target = match request {
            PageRequest::Target(Target::Area(a)) => self.specs.set_of_area(a),
            PageRequest::Target(Target::Set(s)) => self.specs.effective_set(s)?,
            _ => return None,
        };
        (target != self.current_set).then_some(target)
    }

    /// Enter another page set: the next page starts a new contiguous
    /// sequence from it.
    pub fn enter(&mut self, set: usize) {
        self.current_set = set;
        self.run_start = self.pages.len();
        self.used_once.clear();
    }

    /// Whether the next page is the first of the current sequence.
    fn at_run_start(&self) -> bool {
        self.pages.len() == self.run_start
    }

    /// The surface the next page is printed on: odd (front) or even (back),
    /// from the physical surface count. A simplex strategy prints every
    /// page on a front surface.
    fn next_parity(&self) -> Parity {
        if self.specs.sets[self.current_set].relation != Relation::DuplexPaginated {
            return Parity::Odd;
        }
        if self.pages.len().is_multiple_of(2) {
            Parity::Odd
        } else {
            Parity::Even
        }
    }

    /// Add the next page. A parity request on a duplex set that the next
    /// surface does not satisfy first adds a blank filler page (XFA 3.3 §8
    /// "Breaking to a different page within the page set"). Returns the
    /// index of the content page, and the error the fallback stood in for,
    /// if any.
    pub fn next_page(&mut self, request: PageRequest) -> (usize, Option<PaginationError>) {
        let mut error = None;
        if let PageRequest::Parity(wanted) = request
            && self.specs.sets[self.current_set].relation == Relation::DuplexPaginated
            && self.next_parity() != wanted
        {
            let (area, e) = self.choose(None, true);
            error = error.or(e);
            self.push(area, true);
        }
        let target_area = match request {
            PageRequest::Target(Target::Area(a)) => Some(a),
            _ => None,
        };
        let (area, e) = self.choose(target_area, false);
        error = error.or(e);
        self.push(area, false);
        (self.pages.len() - 1, error)
    }

    fn push(&mut self, area: usize, blank: bool) {
        if self.specs.areas[area].position.once() {
            self.used_once.insert(area);
        }
        self.occurrences[area] += 1;
        self.pages.push(PlannedPage { area, blank });
    }

    /// The area of the next page: the named one, or the first of the
    /// current set that matches the layout state.
    fn choose(&self, target: Option<usize>, blank: bool) -> (usize, Option<PaginationError>) {
        if let Some(a) = target {
            return (a, None);
        }
        let set = &self.specs.sets[self.current_set];
        match set.relation {
            Relation::OrderedOccurrence => {
                let next = set.areas.iter().copied().find(|&a| {
                    let spec = &self.specs.areas[a];
                    // A boilerplate page repeating without limit would never
                    // end; the specification asks to treat its max as 1.
                    let max = match spec.max_occur {
                        None if spec.content_areas == 0 => Some(1),
                        m => m,
                    };
                    max.is_none_or(|m| self.occurrences[a] < m)
                });
                match next {
                    Some(a) => (a, None),
                    None => (
                        *set.areas.last().expect("an effective set has areas"),
                        Some(PaginationError::Exhausted {
                            set: set.name.clone(),
                        }),
                    ),
                }
            }
            Relation::SimplexPaginated | Relation::DuplexPaginated => {
                let first = self.at_run_start();
                let parity = self.next_parity();
                let next = set.areas.iter().copied().find(|&a| {
                    let spec = &self.specs.areas[a];
                    if self.used_once.contains(&a) {
                        return false;
                    }
                    let position_ok = match spec.position {
                        PagePosition::Any => true,
                        PagePosition::First => first,
                        PagePosition::Rest => !first,
                        // Known only when the content ends: termination
                        // processing places these.
                        PagePosition::Last | PagePosition::Only => false,
                    };
                    let parity_ok = match spec.odd_or_even {
                        OddOrEven::Any => true,
                        OddOrEven::Odd => parity == Parity::Odd,
                        OddOrEven::Even => parity == Parity::Even,
                    };
                    let blank_ok = match spec.blank {
                        BlankOrNotBlank::Any => true,
                        BlankOrNotBlank::Blank => blank,
                        BlankOrNotBlank::NotBlank => !blank,
                    };
                    position_ok && parity_ok && blank_ok
                });
                match next {
                    Some(a) => (a, None),
                    None => (
                        set.areas[0],
                        Some(PaginationError::NoQualifiedArea {
                            set: set.name.clone(),
                        }),
                    ),
                }
            }
        }
    }

    /// Termination processing for the current page set (XFA 3.3 §8
    /// "Termination processing"), when its content ends: the last page is
    /// redone on the designated `only` page (for a one-page sequence) or
    /// `last` page, when there is one and `fits` says the page's content
    /// fits it; otherwise a blank instance of that page follows. An ordered
    /// set instead appends blank pages until every area has met its
    /// `occur min`.
    pub fn finish(&mut self, mut fits: impl FnMut(usize) -> bool) -> Termination {
        let set = &self.specs.sets[self.current_set];
        if set.relation == Relation::OrderedOccurrence {
            for &a in &set.areas {
                while self.occurrences[a] < self.specs.areas[a].min_occur {
                    self.push(a, true);
                }
            }
            return Termination::Unchanged;
        }
        let run_len = self.pages.len() - self.run_start;
        if run_len == 0 {
            return Termination::Unchanged;
        }
        let last = self.pages.len() - 1;
        let current = &self.specs.areas[self.pages[last].area];
        let with = |position: PagePosition| {
            set.areas
                .iter()
                .copied()
                .find(|&a| self.specs.areas[a].position == position && !self.used_once.contains(&a))
        };
        let candidate = if run_len == 1
            && let Some(only) = with(PagePosition::Only)
        {
            Some(only)
        } else if matches!(current.position, PagePosition::First | PagePosition::Last) {
            // A page that is itself qualified as first or last is kept.
            None
        } else {
            with(PagePosition::Last)
                .filter(|&a| self.specs.areas[a].content_areas == current.content_areas)
        };
        let Some(candidate) = candidate else {
            return Termination::Unchanged;
        };
        if fits(candidate) {
            let replaced = self.pages[last].area;
            self.occurrences[replaced] -= 1;
            self.pages[last].area = candidate;
            self.occurrences[candidate] += 1;
            self.used_once.insert(candidate);
            Termination::Replaced
        } else {
            self.push(candidate, true);
            Termination::BlankAppended
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xfa::num;

    fn leaf(top: f64, bottom: f64) -> Extent {
        Extent::new(num(top), num(bottom), Splitting::Never)
    }

    fn container(top: f64, bottom: f64, children: Vec<Extent>) -> Extent {
        let mut e = Extent::new(
            num(top),
            num(bottom),
            Splitting::Inside {
                margin_top: Num::ZERO,
                margin_bottom: Num::ZERO,
            },
        );
        e.children = children;
        e
    }

    #[test]
    fn a_break_between_objects_stays_where_it_is() {
        let extents = vec![leaf(0.0, 100.0), leaf(100.0, 200.0)];
        assert_eq!(
            split_point(&extents, num(0.0), num(100.0)),
            Some(num(100.0))
        );
    }

    #[test]
    fn an_unsplittable_object_moves_whole_to_the_next_area() {
        let extents = vec![leaf(0.0, 100.0), leaf(100.0, 300.0)];
        // Whatever the distance: 150pt from the desired break.
        assert_eq!(
            split_point(&extents, num(0.0), num(250.0)),
            Some(num(100.0))
        );
    }

    #[test]
    fn a_text_splits_at_its_line_boundaries() {
        let text = Extent::new(
            num(0.0),
            num(100.0),
            Splitting::Between(vec![num(30.0), num(60.0)]),
        );
        let extents = std::slice::from_ref(&text);
        assert_eq!(split_point(extents, num(0.0), num(45.0)), Some(num(30.0)));
        assert_eq!(split_point(extents, num(0.0), num(60.0)), Some(num(60.0)));
        // Above the first boundary only the top is left, which is the start.
        assert_eq!(split_point(extents, num(0.0), num(20.0)), None);
    }

    /// XFA 3.3 §8 Example 8.19: a container splits where all its children
    /// agree, never inside its margins.
    #[test]
    fn a_container_splits_by_consensus_of_its_children() {
        let mut d = container(
            100.0,
            400.0,
            vec![
                leaf(130.0, 230.0), // image E
                leaf(260.0, 360.0), // image F
                Extent::new(
                    num(130.0),
                    num(370.0),
                    Splitting::Between(vec![num(200.0), num(250.0), num(300.0)]),
                ),
            ],
        );
        d.splitting = Splitting::Inside {
            margin_top: num(30.0),
            margin_bottom: num(30.0),
        };
        let extents = vec![leaf(0.0, 100.0), d];
        // Desired inside image F and the text: up to the text's boundary
        // between the images.
        assert_eq!(
            split_point(&extents, num(0.0), num(320.0)),
            Some(num(250.0))
        );
        // Desired in the bottom margin: just above it, where the content ends.
        assert_eq!(
            split_point(&extents, num(0.0), num(385.0)),
            Some(num(370.0))
        );
        // Desired in the top margin: the container moves whole.
        assert_eq!(
            split_point(&extents, num(0.0), num(120.0)),
            Some(num(100.0))
        );
    }

    #[test]
    fn keep_next_takes_the_object_along_with_its_successor() {
        let mut heading = leaf(100.0, 120.0);
        heading.keep_next = true;
        let extents = vec![leaf(0.0, 100.0), heading, leaf(120.0, 300.0)];
        // Between heading and block: forbidden, the heading moves too.
        assert_eq!(
            split_point(&extents, num(0.0), num(120.0)),
            Some(num(100.0))
        );
        assert_eq!(
            split_point(&extents, num(0.0), num(200.0)),
            Some(num(100.0))
        );
    }

    #[test]
    fn keep_previous_keeps_the_object_with_its_predecessor() {
        let mut note = leaf(120.0, 140.0);
        note.keep_previous = true;
        let extents = vec![leaf(0.0, 100.0), leaf(100.0, 120.0), note];
        assert_eq!(
            split_point(&extents, num(0.0), num(120.0)),
            Some(num(100.0))
        );
    }

    #[test]
    fn nothing_fitting_is_reported() {
        let extents = vec![leaf(0.0, 500.0)];
        assert_eq!(split_point(&extents, num(0.0), num(300.0)), None);
    }

    /// XFA 3.3 §8 "Overriding ContentArea Boundaries": the object that
    /// fits nowhere goes on the page whole, and the next content area
    /// starts after it.
    #[test]
    fn an_overridden_boundary_ends_after_the_object_that_did_not_fit() {
        let extents = vec![leaf(0.0, 500.0), leaf(500.0, 600.0)];
        assert_eq!(split_point_below(&extents, num(300.0)), num(500.0));
        // A positioned block (never split) holding a text: its bottom.
        let mut block = container(0.0, 500.0, vec![leaf(10.0, 490.0)]);
        block.splitting = Splitting::Never;
        assert_eq!(split_point_below(&[block], num(300.0)), num(500.0));
        // A splittable container: the first boundary its children accept.
        let flowed = container(0.0, 500.0, vec![leaf(0.0, 350.0), leaf(350.0, 500.0)]);
        assert_eq!(split_point_below(&[flowed], num(300.0)), num(350.0));
    }

    #[test]
    fn forced_breaks_are_in_column_order_and_merged_at_one_position() {
        let request = |t: BreakTarget, target: Option<&str>, start_new: bool| BreakRequest {
            target_type: t,
            target: target.map(String::from),
            start_new,
        };
        let mut a = leaf(0.0, 100.0);
        a.break_after = Some(request(BreakTarget::ContentArea, None, false));
        let mut b = leaf(100.0, 200.0);
        b.break_before = Some(request(BreakTarget::PageArea, Some("MP"), true));
        let mut c = container(
            200.0,
            300.0,
            vec![{
                let mut inner = leaf(250.0, 300.0);
                inner.break_before = Some(request(BreakTarget::PageOdd, None, false));
                inner
            }],
        );
        c.break_before = None;
        let breaks = forced_breaks(&[a, b, c]);
        assert_eq!(
            breaks,
            vec![
                ForcedBreak {
                    at: num(100.0),
                    request: request(BreakTarget::PageArea, Some("MP"), true)
                },
                ForcedBreak {
                    at: num(250.0),
                    request: request(BreakTarget::PageOdd, None, false)
                },
            ]
        );
    }

    fn area(name: &str, position: PagePosition) -> AreaSpec {
        AreaSpec {
            name: Some(name.to_string()),
            position,
            odd_or_even: OddOrEven::Any,
            blank: BlankOrNotBlank::Any,
            min_occur: 1,
            max_occur: None,
            content_areas: 1,
        }
    }

    fn one_set(relation: Relation, areas: Vec<AreaSpec>) -> PageSets {
        let n = areas.len();
        PageSets {
            areas,
            sets: vec![SetSpec {
                name: Some("S".into()),
                relation,
                areas: (0..n).collect(),
                children: vec![],
            }],
        }
    }

    fn names(p: &Paginator<'_>) -> Vec<(String, bool)> {
        p.pages()
            .iter()
            .map(|pg| (p.specs.areas[pg.area].name.clone().unwrap(), pg.blank))
            .collect()
    }

    /// XFA 3.3 §8 Example 8.25: `rest` cannot serve the first page.
    #[test]
    fn a_first_page_is_chosen_over_rest_by_document_order_with_qualification() {
        let specs = one_set(
            Relation::SimplexPaginated,
            vec![
                area("A", PagePosition::Rest),
                area("B", PagePosition::First),
            ],
        );
        let mut p = Paginator::new(&specs, None).unwrap();
        for _ in 0..3 {
            let (_, err) = p.next_page(PageRequest::Next);
            assert_eq!(err, None);
        }
        assert_eq!(
            names(&p),
            vec![
                ("B".into(), false),
                ("A".into(), false),
                ("A".into(), false)
            ]
        );
    }

    /// XFA 3.3 §8 Example 8.26: odd and even surfaces alternate in duplex.
    #[test]
    fn duplex_pages_alternate_between_odd_and_even_areas() {
        let mut a = area("A", PagePosition::Rest);
        a.odd_or_even = OddOrEven::Odd;
        let mut b = area("B", PagePosition::Rest);
        b.odd_or_even = OddOrEven::Even;
        let specs = one_set(
            Relation::DuplexPaginated,
            vec![a, b, area("C", PagePosition::First)],
        );
        let mut p = Paginator::new(&specs, None).unwrap();
        for _ in 0..4 {
            p.next_page(PageRequest::Next);
        }
        assert_eq!(
            names(&p).into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
            vec!["C", "B", "A", "B"]
        );
    }

    /// XFA 3.3 §8 Example 8.27: a break to an odd page from an odd page
    /// inserts a blank even page. (The example's own areas are `rest`,
    /// which leaves no area for a first page; `any` here.)
    #[test]
    fn a_break_to_an_odd_page_from_an_odd_page_adds_a_blank_filler() {
        let mut a = area("A", PagePosition::Any);
        a.odd_or_even = OddOrEven::Odd;
        let mut b = area("B", PagePosition::Any);
        b.odd_or_even = OddOrEven::Even;
        let specs = one_set(Relation::DuplexPaginated, vec![a, b]);
        let mut p = Paginator::new(&specs, None).unwrap();
        // Three pages of Y: A, B, A.
        for _ in 0..3 {
            assert_eq!(p.next_page(PageRequest::Next).1, None);
        }
        // breakAfter pageOdd: filler B, then A for Z.
        let (page, err) = p.next_page(PageRequest::Parity(Parity::Odd));
        assert_eq!(err, None);
        assert_eq!(page, 4);
        assert_eq!(
            names(&p),
            vec![
                ("A".into(), false),
                ("B".into(), false),
                ("A".into(), false),
                ("B".into(), true),
                ("A".into(), false)
            ]
        );
    }

    /// XFA 3.3 §8 Example 8.28: the last page is redone on the designated
    /// last page when its content fits, and a blank one follows otherwise.
    #[test]
    fn termination_uses_the_designated_last_page() {
        let specs = one_set(
            Relation::SimplexPaginated,
            vec![area("A", PagePosition::Any), area("B", PagePosition::Last)],
        );
        let mut p = Paginator::new(&specs, None).unwrap();
        for _ in 0..3 {
            p.next_page(PageRequest::Next);
        }
        assert_eq!(p.finish(|_| true), Termination::Replaced);
        assert_eq!(
            names(&p),
            vec![
                ("A".into(), false),
                ("A".into(), false),
                ("B".into(), false)
            ]
        );

        let mut p = Paginator::new(&specs, None).unwrap();
        p.next_page(PageRequest::Next);
        assert_eq!(p.finish(|_| false), Termination::BlankAppended);
        assert_eq!(names(&p), vec![("A".into(), false), ("B".into(), true)]);
    }

    /// XFA 3.3 §8 Example 8.29: `only` takes precedence over `first` for a
    /// one-page document.
    #[test]
    fn a_single_page_uses_the_only_page() {
        let specs = one_set(
            Relation::SimplexPaginated,
            vec![
                area("A", PagePosition::First),
                area("B", PagePosition::Any),
                area("C", PagePosition::Last),
                area("D", PagePosition::Only),
            ],
        );
        let mut p = Paginator::new(&specs, None).unwrap();
        p.next_page(PageRequest::Next);
        assert_eq!(p.finish(|_| true), Termination::Replaced);
        assert_eq!(names(&p), vec![("D".into(), false)]);

        // Two pages: A first, then B, and C replaces the second.
        let mut p = Paginator::new(&specs, None).unwrap();
        p.next_page(PageRequest::Next);
        p.next_page(PageRequest::Next);
        assert_eq!(p.finish(|_| true), Termination::Replaced);
        assert_eq!(
            names(&p).into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
            vec!["A", "C"]
        );
    }

    #[test]
    fn a_first_page_is_never_replaced_by_the_last_page() {
        let specs = one_set(
            Relation::SimplexPaginated,
            vec![
                area("A", PagePosition::First),
                area("B", PagePosition::Rest),
                area("C", PagePosition::Last),
            ],
        );
        let mut p = Paginator::new(&specs, None).unwrap();
        p.next_page(PageRequest::Next);
        assert_eq!(p.finish(|_| true), Termination::Unchanged);
        assert_eq!(names(&p), vec![("A".into(), false)]);
    }

    #[test]
    fn no_qualified_area_falls_back_to_the_first_with_an_error() {
        let specs = one_set(
            Relation::SimplexPaginated,
            vec![area("A", PagePosition::Rest)],
        );
        let mut p = Paginator::new(&specs, None).unwrap();
        let (_, err) = p.next_page(PageRequest::Next);
        assert_eq!(
            err,
            Some(PaginationError::NoQualifiedArea {
                set: Some("S".into())
            })
        );
        assert_eq!(names(&p), vec![("A".into(), false)]);
    }

    #[test]
    fn ordered_pagination_uses_areas_in_document_order_up_to_their_maximum() {
        let mut a = area("A", PagePosition::Any);
        a.max_occur = Some(2);
        let specs = one_set(
            Relation::OrderedOccurrence,
            vec![a, area("B", PagePosition::Any)],
        );
        let mut p = Paginator::new(&specs, None).unwrap();
        for _ in 0..4 {
            assert_eq!(p.next_page(PageRequest::Next).1, None);
        }
        assert_eq!(
            names(&p).into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
            vec!["A", "A", "B", "B"]
        );
    }

    #[test]
    fn ordered_pagination_fills_the_minimum_occurrence_with_blank_pages() {
        let mut a = area("A", PagePosition::Any);
        a.min_occur = 2;
        let specs = one_set(Relation::OrderedOccurrence, vec![a]);
        let mut p = Paginator::new(&specs, None).unwrap();
        p.next_page(PageRequest::Next);
        p.finish(|_| true);
        assert_eq!(names(&p), vec![("A".into(), false), ("A".into(), true)]);
    }

    #[test]
    fn a_target_in_another_set_changes_the_set_and_starts_a_new_sequence() {
        let specs = PageSets {
            areas: vec![
                area("MP", PagePosition::Any),
                area("MP_Last", PagePosition::Last),
                area("Annex", PagePosition::Any),
            ],
            sets: vec![
                SetSpec {
                    name: Some("MPs".into()),
                    relation: Relation::DuplexPaginated,
                    areas: vec![],
                    children: vec![1, 2],
                },
                SetSpec {
                    name: Some("Main".into()),
                    relation: Relation::DuplexPaginated,
                    areas: vec![0, 1],
                    children: vec![],
                },
                SetSpec {
                    name: Some("AnnexSet".into()),
                    relation: Relation::DuplexPaginated,
                    areas: vec![2],
                    children: vec![],
                },
            ],
        };
        // Entering the outer set means entering its first child.
        let mut p = Paginator::new(&specs, None).unwrap();
        assert_eq!(p.current_set(), 1);
        p.next_page(PageRequest::Next);
        let target = specs.resolve_target("MPs_Annex.Annex").unwrap();
        assert_eq!(target, Target::Area(2));
        let change = p.set_change(PageRequest::Target(target)).unwrap();
        assert_eq!(p.finish(|_| true), Termination::Replaced);
        p.enter(change);
        p.next_page(PageRequest::Target(target));
        assert_eq!(p.finish(|_| true), Termination::Unchanged);
        assert_eq!(
            names(&p).into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
            vec!["MP_Last", "Annex"]
        );
    }
}
