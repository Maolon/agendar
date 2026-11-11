//! Human-wait replay over the accepted dependency history, near-linear.
//!
//! Semantics (see `eta`): every winning `depends_on` patch is one transition.
//! Between transitions the accepted graph is the declared graph minus the edges
//! that would close a cycle, chosen greedily in downstream-node order and then
//! declaration order. An ask on feature `u` holds a downstream feature `n` over
//! each transition span during which `u` is an accepted transitive prerequisite
//! of `n`, clipped to the ask, skipped when `u` was satisfied at the span start
//! and ended by `u`'s next completion.
//!
//! The interval of one span does not depend on `n`, so each ask computes its
//! span intervals once and then only tracks which features descend from `u` at
//! those spans. Descendants are kept incrementally while the transitions are
//! swept. Consumers only measure the union of a feature's intervals (an open
//! one ends at `as_of`) and ask whether one is open, so each feature's list is
//! reduced to an equivalent minimal one: merged closed intervals plus the
//! earliest open interval.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::model::ids::{NodePath, UtcTime};
use crate::reduce::eta::{WaitAsk, completion_after, satisfied_at};
use crate::reduce::plan::FeatureFold;

/// One human-wait interval; `None` is still open.
pub type Interval = (UtcTime, Option<UtcTime>);

/// The replayed human-wait intervals of every feature.
///
/// For each feature the union of its intervals, and whether any is open, equal
/// what a per-transition replay of the full closure of every feature yields.
pub fn by_node(
    features: &BTreeMap<NodePath, FeatureFold>,
    asks: &[WaitAsk],
) -> BTreeMap<NodePath, Vec<Interval>> {
    let nodes: Vec<&NodePath> = features.keys().collect();
    let id_of: BTreeMap<&NodePath, usize> = nodes
        .iter()
        .enumerate()
        .map(|(id, node)| (*node, id))
        .collect();
    let transitions = Transitions::new(features, &id_of);

    let mut trackers: Vec<Option<Tracker>> = Vec::with_capacity(asks.len());
    for ask in asks {
        let tracker = id_of.get(&ask.node).and_then(|&upstream| {
            let feature = &features[&ask.node];
            let pieces = transitions.pieces(ask, feature);
            (!pieces.is_empty()).then(|| Tracker::new(upstream, pieces))
        });
        trackers.push(tracker);
    }
    let mut starts: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (index, tracker) in trackers.iter().enumerate() {
        if let Some(tracker) = tracker {
            starts.entry(tracker.pieces[0].0).or_default().push(index);
        }
    }

    let mut graph = DeclaredGraph::new(nodes.len());
    let mut active: Vec<usize> = Vec::new();
    for (step, changes) in transitions.changes.iter().enumerate() {
        let mut applied = Vec::with_capacity(changes.len());
        for (node, upstreams) in changes {
            applied.push(graph.declare(*node, upstreams.clone()));
        }
        for &index in &active {
            if let Some(tracker) = trackers[index].as_mut() {
                match applied.as_slice() {
                    [change] => tracker.apply(change, &graph),
                    several => {
                        if several.iter().any(|change| tracker.touches(change)) {
                            tracker.recompute(&graph);
                        }
                    }
                }
            }
        }
        if let Some(new) = starts.get(&step) {
            for &index in new {
                if let Some(tracker) = trackers[index].as_mut() {
                    tracker.recompute(&graph);
                }
                active.push(index);
            }
        }
        active.retain(|&index| {
            let Some(tracker) = trackers[index].as_mut() else {
                return false;
            };
            tracker.record(step)
        });
    }

    let mut waits: BTreeMap<NodePath, Vec<Interval>> = nodes
        .iter()
        .map(|node| ((*node).clone(), Vec::new()))
        .collect();
    for (ask, tracker) in asks.iter().zip(&trackers) {
        if let Some(own) = waits.get_mut(&ask.node) {
            own.push((ask.start, ask.end));
        }
        let Some(tracker) = tracker else {
            continue;
        };
        for &(node, from, to) in &tracker.runs {
            let target = waits.get_mut(nodes[node]).expect("known node");
            tracker.components(from, to, target);
        }
    }
    for intervals in waits.values_mut() {
        minimize(intervals);
    }
    waits
}

/// Replace one feature's intervals by the smallest list with the same union
/// for every clip window and the same "any open" answer: the union of the
/// closed intervals as disjoint sorted intervals, then the earliest open one.
/// An open interval reads as ending at `as_of`, so several of them cover
/// exactly what the earliest covers; closed ones never merge into it.
fn minimize(intervals: &mut Vec<Interval>) {
    let mut closed: Vec<(UtcTime, UtcTime)> = intervals
        .iter()
        .filter_map(|(start, end)| end.map(|end| (*start, end)))
        .filter(|(start, end)| end.millis() > start.millis())
        .collect();
    let open = intervals
        .iter()
        .filter(|(_, end)| end.is_none())
        .map(|(start, _)| *start)
        .min();
    closed.sort();
    intervals.clear();
    let mut current: Option<(UtcTime, UtcTime)> = None;
    for (start, end) in closed {
        current = match current {
            Some((from, to)) if start <= to => Some((from, to.max(end))),
            Some((from, to)) => {
                intervals.push((from, Some(to)));
                Some((start, end))
            }
            None => Some((start, end)),
        };
    }
    if let Some((from, to)) = current {
        intervals.push((from, Some(to)));
    }
    if let Some(start) = open {
        intervals.push((start, None));
    }
}

/// The dependency transitions in causal order.
struct Transitions {
    /// Effective time of each transition, forced monotonic.
    at: Vec<UtcTime>,
    /// Per transition: the features whose declaration changed and their valid
    /// upstreams.
    changes: Vec<Vec<Declaration>>,
}

/// One feature and its valid upstreams (known, not itself) in declaration
/// order.
type Declaration = (usize, Vec<usize>);

impl Transitions {
    fn new(features: &BTreeMap<NodePath, FeatureFold>, id_of: &BTreeMap<&NodePath, usize>) -> Self {
        let mut updates: BTreeMap<usize, (UtcTime, Vec<Declaration>)> = BTreeMap::new();
        for (node, feature) in features {
            let id = id_of[node];
            for change in &feature.depends_on_history {
                let upstreams = change
                    .depends_on
                    .iter()
                    .filter(|upstream| *upstream != node)
                    .filter_map(|upstream| id_of.get(upstream).copied())
                    .collect();
                updates
                    .entry(change.order)
                    .or_insert((change.ts, Vec::new()))
                    .1
                    .push((id, upstreams));
            }
        }
        let mut at = Vec::with_capacity(updates.len());
        let mut changes = Vec::with_capacity(updates.len());
        let mut last: Option<UtcTime> = None;
        for (ts, list) in updates.into_values() {
            // Keep the transition times monotonic even when replicated clocks
            // skew: an interval whose start is after its end would vanish.
            let time = last.map_or(ts, |previous| ts.max(previous));
            last = Some(time);
            at.push(time);
            changes.push(list);
        }
        Self { at, changes }
    }

    /// The interval each transition span contributes for one ask whose
    /// upstream is `feature`, in transition order. Spans that end before the
    /// ask starts or start after it ends are skipped without being visited.
    fn pieces(&self, ask: &WaitAsk, feature: &FeatureFold) -> Vec<(usize, Interval)> {
        let count = self.at.len();
        if count == 0 {
            return Vec::new();
        }
        let first = self.at[1..].partition_point(|time| time.millis() <= ask.start.millis());
        let last = ask.end.map_or(count, |end| {
            self.at.partition_point(|time| time.millis() < end.millis())
        });
        let mut pieces = Vec::new();
        for step in first..last {
            let span_start = self.at[step];
            let span_end = self.at.get(step + 1).copied();
            let start = span_start.max(ask.start);
            let clipped = match (span_end, ask.end) {
                (Some(end), Some(ask_end)) => Some(end.min(ask_end)),
                (Some(end), None) => Some(end),
                (None, Some(ask_end)) => Some(ask_end),
                (None, None) => None,
            };
            if clipped.is_some_and(|end| end.millis() <= start.millis()) {
                continue;
            }
            if satisfied_at(feature, start) {
                continue;
            }
            let end = match (clipped, completion_after(feature, start)) {
                (Some(end), Some(resolved)) => Some(end.min(resolved)),
                (Some(end), None) => Some(end),
                (None, Some(resolved)) => Some(resolved),
                (None, None) => None,
            };
            if end.is_some_and(|end| end.millis() <= start.millis()) {
                continue;
            }
            pieces.push((step, (start, end)));
        }
        pieces
    }
}

/// One feature's declaration change as applied to the graph.
struct Change {
    node: usize,
    before: Vec<usize>,
    after: Vec<usize>,
}

/// The declared (not yet cycle-filtered) dependency graph at one transition.
struct DeclaredGraph {
    declared: Vec<Vec<usize>>,
    downstreams: Vec<BTreeSet<usize>>,
}

impl DeclaredGraph {
    fn new(nodes: usize) -> Self {
        Self {
            declared: vec![Vec::new(); nodes],
            downstreams: vec![BTreeSet::new(); nodes],
        }
    }

    fn declare(&mut self, node: usize, upstreams: Vec<usize>) -> Change {
        let before = std::mem::replace(&mut self.declared[node], upstreams.clone());
        let old: BTreeSet<usize> = before.iter().copied().collect();
        let new: BTreeSet<usize> = upstreams.iter().copied().collect();
        for upstream in old.difference(&new) {
            self.downstreams[*upstream].remove(&node);
        }
        for upstream in new.difference(&old) {
            self.downstreams[*upstream].insert(node);
        }
        Change {
            node,
            before,
            after: upstreams,
        }
    }

    /// Every node reachable from `start` along declared edges, `start` included.
    fn reach(&self, start: usize) -> BTreeSet<usize> {
        let mut seen = BTreeSet::from([start]);
        let mut queue = VecDeque::from([start]);
        while let Some(node) = queue.pop_front() {
            for &next in &self.downstreams[node] {
                if seen.insert(next) {
                    queue.push_back(next);
                }
            }
        }
        seen
    }
}

/// Marks a node that is not a current descendant in `Tracker::joined_at`.
const ABSENT: usize = usize::MAX;

/// Descendant tracking for one ask while its spans are swept.
struct Tracker {
    upstream: usize,
    pieces: Vec<(usize, Interval)>,
    /// Pieces recorded so far.
    recorded: usize,
    /// Forward reach of `upstream` in the declared graph, upstream included,
    /// as a membership table and a list.
    in_region: Vec<bool>,
    region: Vec<usize>,
    /// Whether the declared graph has no cycle inside the region; then every
    /// declared edge there is accepted and the descendants are the region.
    acyclic: bool,
    /// Per node, the piece a current accepted descendant joined at.
    joined_at: Vec<usize>,
    /// The current accepted descendants of `upstream`.
    members: Vec<usize>,
    /// Finished runs: node and the piece range `[from, to)` it descended in.
    runs: Vec<(usize, usize, usize)>,
}

impl Tracker {
    fn new(upstream: usize, pieces: Vec<(usize, Interval)>) -> Self {
        Self {
            upstream,
            pieces,
            recorded: 0,
            in_region: Vec::new(),
            region: Vec::new(),
            acyclic: true,
            joined_at: Vec::new(),
            members: Vec::new(),
            runs: Vec::new(),
        }
    }

    /// Whether a declaration change can alter the region: it is inside it, or
    /// it now depends on a node inside it. A node outside the forward-closed
    /// region had no edge from it before, so nothing else can matter.
    fn touches(&self, change: &Change) -> bool {
        self.in_region[change.node]
            || change
                .after
                .iter()
                .any(|upstream| self.in_region[*upstream])
    }

    /// Follow the only declaration change of one transition made after the
    /// tracker started.
    fn apply(&mut self, change: &Change, graph: &DeclaredGraph) {
        if !self.touches(change) {
            return;
        }
        if self.in_region[change.node] {
            if change.before != change.after {
                self.recompute(graph);
            }
            return;
        }
        // A node outside the region joins it with everything it reaches. If
        // that part is acyclic and does not lead back into the region, no
        // component inside the region changes, so no acceptance there changes;
        // the new edges into the node and every edge inside the part are
        // accepted. The part descends when the node now depends on the
        // tracked feature or one of its accepted descendants.
        let added = graph.reach(change.node);
        if added.iter().any(|node| self.in_region[*node]) || !acyclic(graph, &added) {
            self.recompute(graph);
            return;
        }
        let entered = change
            .after
            .iter()
            .any(|upstream| *upstream == self.upstream || self.joined_at[*upstream] != ABSENT);
        for node in added {
            self.in_region[node] = true;
            self.region.push(node);
            if entered {
                self.joined_at[node] = self.recorded;
                self.members.push(node);
            }
        }
    }

    /// Recompute the accepted descendants from the current graph.
    fn recompute(&mut self, graph: &DeclaredGraph) {
        let nodes = graph.declared.len();
        if self.joined_at.is_empty() {
            self.joined_at = vec![ABSENT; nodes];
        }
        let mut in_region = vec![false; nodes];
        let mut region = vec![self.upstream];
        in_region[self.upstream] = true;
        let mut next = 0;
        while let Some(&node) = region.get(next) {
            next += 1;
            for &child in &graph.downstreams[node] {
                if !in_region[child] {
                    in_region[child] = true;
                    region.push(child);
                }
            }
        }
        let cyclic = cyclic_part(graph, &region);
        self.acyclic = cyclic.is_empty();
        let descendants: Vec<usize> = if self.acyclic {
            region
                .iter()
                .copied()
                .filter(|node| *node != self.upstream)
                .collect()
        } else {
            accepted_descendants(graph, self.upstream, &cyclic)
        };
        let mut keep = vec![false; nodes];
        for &node in &descendants {
            keep[node] = true;
        }
        for &node in &self.members {
            if !keep[node] {
                let from = std::mem::replace(&mut self.joined_at[node], ABSENT);
                if from < self.recorded {
                    self.runs.push((node, from, self.recorded));
                }
            }
        }
        for &node in &descendants {
            if self.joined_at[node] == ABSENT {
                self.joined_at[node] = self.recorded;
            }
        }
        self.members = descendants;
        self.in_region = in_region;
        self.region = region;
    }

    /// Record the piece of `step` if it has one. Returns whether the tracker
    /// still has pieces to come; a finished tracker closes its runs and drops
    /// its tables.
    fn record(&mut self, step: usize) -> bool {
        if self.pieces.get(self.recorded).map(|(at, _)| *at) == Some(step) {
            self.recorded += 1;
        }
        if self.recorded < self.pieces.len() {
            return true;
        }
        for node in std::mem::take(&mut self.members) {
            let from = self.joined_at[node];
            if from < self.recorded {
                self.runs.push((node, from, self.recorded));
            }
        }
        self.joined_at = Vec::new();
        self.in_region = Vec::new();
        self.region = Vec::new();
        false
    }

    /// Push the merged intervals of pieces `[from, to)`: consecutive closed
    /// pieces that touch become one interval; an open piece stays separate.
    fn components(&self, from: usize, to: usize, target: &mut Vec<Interval>) {
        let mut current: Option<Interval> = None;
        for (_, piece) in &self.pieces[from..to] {
            match (current, piece.1) {
                (Some((start, Some(end))), Some(next_end)) if piece.0 == end => {
                    current = Some((start, Some(next_end)));
                }
                _ => {
                    if let Some(done) = current.take() {
                        target.push(done);
                    }
                    current = Some(*piece);
                }
            }
        }
        if let Some(done) = current {
            target.push(done);
        }
    }
}

/// Whether the declared graph restricted to `nodes` has no cycle.
fn acyclic(graph: &DeclaredGraph, nodes: &BTreeSet<usize>) -> bool {
    let mut indegree: BTreeMap<usize, usize> = nodes.iter().map(|&node| (node, 0)).collect();
    for &node in nodes {
        for next in &graph.downstreams[node] {
            if let Some(degree) = indegree.get_mut(next) {
                *degree += 1;
            }
        }
    }
    let mut ready: Vec<usize> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(node, _)| *node)
        .collect();
    let mut removed = 0;
    while let Some(node) = ready.pop() {
        removed += 1;
        for next in &graph.downstreams[node] {
            if let Some(degree) = indegree.get_mut(next) {
                *degree -= 1;
                if *degree == 0 {
                    ready.push(*next);
                }
            }
        }
    }
    removed == nodes.len()
}

/// The nodes of the forward-closed `region` that Kahn's algorithm cannot
/// remove: those on a cycle of the declared graph or downstream of one. Empty
/// exactly when the region is acyclic.
fn cyclic_part(graph: &DeclaredGraph, region: &[usize]) -> Vec<usize> {
    let mut indegree = vec![0_usize; graph.declared.len()];
    for &node in region {
        for &child in &graph.downstreams[node] {
            indegree[child] += 1;
        }
    }
    let mut ready: Vec<usize> = region
        .iter()
        .copied()
        .filter(|node| indegree[*node] == 0)
        .collect();
    while let Some(node) = ready.pop() {
        for &child in &graph.downstreams[node] {
            indegree[child] -= 1;
            if indegree[child] == 0 {
                ready.push(child);
            }
        }
    }
    region
        .iter()
        .copied()
        .filter(|node| indegree[*node] > 0)
        .collect()
}

/// Accepted descendants of `upstream` when its region has a cycle; `cyclic`
/// is [`cyclic_part`] of that region.
///
/// An edge whose ends lie in different strongly connected components of the
/// declared graph can never close a cycle, so it is always accepted. Whether an
/// edge inside a component closes a cycle depends only on the edges of that
/// component accepted before it, so the greedy rule (downstream-node order,
/// then declaration order) runs per component and gives the global result.
/// Every cycle lies inside `cyclic`, so its components are the ones to check.
fn accepted_descendants(graph: &DeclaredGraph, upstream: usize, cyclic: &[usize]) -> Vec<usize> {
    let (component_of, components) = strongly_connected(graph, cyclic);
    let mut rejected: BTreeSet<(usize, usize)> = BTreeSet::new();
    for members in components.iter().filter(|members| members.len() > 1) {
        let component = component_of[members[0]];
        // `members` is sorted by node id, which is node-path order.
        let mut inner: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
        for &downstream in members {
            for &candidate in &graph.declared[downstream] {
                if component_of[candidate] != component {
                    continue;
                }
                if reaches(&inner, downstream, candidate) {
                    rejected.insert((candidate, downstream));
                    continue;
                }
                inner.entry(candidate).or_default().insert(downstream);
            }
        }
    }
    let mut seen = vec![false; graph.declared.len()];
    seen[upstream] = true;
    let mut descendants = Vec::new();
    let mut queue = VecDeque::from([upstream]);
    while let Some(node) = queue.pop_front() {
        for &child in &graph.downstreams[node] {
            if !seen[child] && !rejected.contains(&(node, child)) {
                seen[child] = true;
                descendants.push(child);
                queue.push_back(child);
            }
        }
    }
    descendants
}

/// Is `to` reachable from `from` (or equal) along `edges`?
fn reaches(edges: &BTreeMap<usize, BTreeSet<usize>>, from: usize, to: usize) -> bool {
    let mut seen = BTreeSet::from([from]);
    let mut stack = vec![from];
    while let Some(node) = stack.pop() {
        if node == to {
            return true;
        }
        if let Some(next) = edges.get(&node) {
            for &child in next {
                if seen.insert(child) {
                    stack.push(child);
                }
            }
        }
    }
    false
}

/// Strongly connected components of the declared graph restricted to `nodes`
/// (iterative Tarjan): the component of every node (`ABSENT` outside
/// `nodes`) and each component's members sorted by node id.
fn strongly_connected(graph: &DeclaredGraph, nodes: &[usize]) -> (Vec<usize>, Vec<Vec<usize>>) {
    let size = graph.declared.len();
    let mut inside = vec![false; size];
    for &node in nodes {
        inside[node] = true;
    }
    let mut index_of = vec![ABSENT; size];
    let mut lowlink = vec![0_usize; size];
    let mut on_stack = vec![false; size];
    let mut component_of = vec![ABSENT; size];
    let mut components: Vec<Vec<usize>> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut counter = 0;
    let children = |node: usize| -> Vec<usize> {
        graph.downstreams[node]
            .iter()
            .copied()
            .filter(|child| inside[*child])
            .collect()
    };
    for &root in nodes {
        if index_of[root] != ABSENT {
            continue;
        }
        index_of[root] = counter;
        lowlink[root] = counter;
        counter += 1;
        stack.push(root);
        on_stack[root] = true;
        let mut work: Vec<(usize, usize, Vec<usize>)> = vec![(root, 0, children(root))];
        while let Some((node, next, list)) = work.last_mut() {
            let node = *node;
            if let Some(&child) = list.get(*next) {
                *next += 1;
                if index_of[child] == ABSENT {
                    index_of[child] = counter;
                    lowlink[child] = counter;
                    counter += 1;
                    stack.push(child);
                    on_stack[child] = true;
                    work.push((child, 0, children(child)));
                } else if on_stack[child] {
                    lowlink[node] = lowlink[node].min(index_of[child]);
                }
            } else {
                let low = lowlink[node];
                if low == index_of[node] {
                    let mut members = Vec::new();
                    while let Some(top) = stack.pop() {
                        on_stack[top] = false;
                        component_of[top] = components.len();
                        members.push(top);
                        if top == node {
                            break;
                        }
                    }
                    members.sort_unstable();
                    components.push(members);
                }
                work.pop();
                if let Some((parent, _, _)) = work.last() {
                    lowlink[*parent] = lowlink[*parent].min(low);
                }
            }
        }
    }
    (component_of, components)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{Interval, by_node};
    use crate::model::event::WorkStatus;
    use crate::model::ids::{EtaRange, EventId, Harness, NodePath, Seconds, SessionId, UtcTime};
    use crate::reduce::eta::{WaitAsk, union_millis};
    use crate::reduce::plan::{AttemptFold, DependencyChange, FeatureFold};

    /// The replay this module replaced: every known feature's full closure at
    /// every transition, then every span of every ask. Kept as the oracle.
    mod reference {
        use std::collections::{BTreeMap, BTreeSet};

        use crate::model::ids::{NodePath, UtcTime};
        use crate::reduce::eta::{WaitAsk, completion_after, satisfied_at};
        use crate::reduce::graph;
        use crate::reduce::plan::FeatureFold;

        pub fn by_node(
            features: &BTreeMap<NodePath, FeatureFold>,
            asks: &[WaitAsk],
        ) -> BTreeMap<NodePath, Vec<(UtcTime, Option<UtcTime>)>> {
            let known: BTreeSet<NodePath> = features.keys().cloned().collect();
            let steps = dependency_steps(features, &known);
            let none = Vec::new();
            features
                .keys()
                .map(|node| {
                    let own = steps.get(node).unwrap_or(&none);
                    (node.clone(), waits(node, own, asks, features))
                })
                .collect()
        }

        /// One accepted-dependency snapshot of one feature, effective from `at` until
        /// the next step (or forever). `upstreams` is the transitive closure of the
        /// accepted edges at that moment.
        #[derive(Debug, Clone, PartialEq, Eq)]
        struct DependencyStep {
            at: UtcTime,
            upstreams: BTreeSet<NodePath>,
        }

        /// Reconstruct the accepted dependency graph over causal time.
        ///
        /// Each winning `depends_on` patch is one transition. Between transitions the
        /// map holds the transitive upstream closure of every known node, so a wait can
        /// ask which prerequisite edges were live when a human was asked. A dependency
        /// that is added later never backdates a hold, and one that is removed later
        /// closes an open hold at the removal instead of erasing it.
        fn dependency_steps(
            features: &BTreeMap<NodePath, FeatureFold>,
            known: &BTreeSet<NodePath>,
        ) -> BTreeMap<NodePath, Vec<DependencyStep>> {
            struct Transition {
                ts: UtcTime,
                changes: Vec<(NodePath, Vec<NodePath>)>,
            }
            let mut updates: BTreeMap<usize, Transition> = BTreeMap::new();
            for (node, feature) in features {
                for change in &feature.depends_on_history {
                    let entry = updates.entry(change.order).or_insert(Transition {
                        ts: change.ts,
                        changes: Vec::new(),
                    });
                    entry
                        .changes
                        .push((node.clone(), change.depends_on.clone()));
                }
            }
            let mut declared: BTreeMap<NodePath, Vec<NodePath>> = BTreeMap::new();
            let mut steps: BTreeMap<NodePath, Vec<DependencyStep>> = BTreeMap::new();
            let mut last_at: Option<UtcTime> = None;
            for (_, transition) in updates {
                for (node, depends_on) in transition.changes {
                    declared.insert(node, depends_on);
                }
                // Keep the transition times monotonic even when replicated clocks skew:
                // an interval whose start is after its end would silently vanish.
                let at = last_at.map_or(transition.ts, |previous| transition.ts.max(previous));
                last_at = Some(at);
                for (node, upstreams) in accepted_closures(&declared, known) {
                    steps
                        .entry(node)
                        .or_default()
                        .push(DependencyStep { at, upstreams });
                }
            }
            steps
        }

        /// Accepted edges and their transitive upstream closure at one snapshot.
        fn accepted_closures(
            declared: &BTreeMap<NodePath, Vec<NodePath>>,
            known: &BTreeSet<NodePath>,
        ) -> BTreeMap<NodePath, BTreeSet<NodePath>> {
            let mut adjacency: BTreeMap<NodePath, BTreeSet<NodePath>> = BTreeMap::new();
            for (downstream, upstreams) in declared {
                for upstream in upstreams {
                    if upstream == downstream || !known.contains(upstream) {
                        continue;
                    }
                    if graph::reachable(&adjacency, downstream, upstream) {
                        continue;
                    }
                    adjacency
                        .entry(upstream.clone())
                        .or_default()
                        .insert(downstream.clone());
                }
            }
            // One reversed view serves every node's ancestor walk.
            let mut reverse: BTreeMap<NodePath, BTreeSet<NodePath>> = BTreeMap::new();
            for (upstream, downstreams) in &adjacency {
                for downstream in downstreams {
                    reverse
                        .entry(downstream.clone())
                        .or_default()
                        .insert(upstream.clone());
                }
            }
            known
                .iter()
                .map(|node| {
                    let upstreams = graph::descendants(&reverse, node).into_iter().collect();
                    (node.clone(), upstreams)
                })
                .collect()
        }

        /// Periods during which `upstream` was an accepted (transitive) prerequisite of
        /// the feature described by `steps`.
        fn upstream_spans(
            steps: &[DependencyStep],
            upstream: &NodePath,
        ) -> Vec<(UtcTime, Option<UtcTime>)> {
            let mut spans = Vec::new();
            for (index, step) in steps.iter().enumerate() {
                if !step.upstreams.contains(upstream) {
                    continue;
                }
                spans.push((step.at, steps.get(index + 1).map(|next| next.at)));
            }
            spans
        }

        /// Reconstruct the human-wait intervals of one feature.
        ///
        /// An ask on the feature's own node holds it until the ask is cleared. An ask on
        /// a prerequisite holds it only while that dependency edge is live and the
        /// prerequisite is still pending; the hold ends at the answer, at the edge
        /// removal or at the prerequisite's completion transition, whichever is first.
        /// Closed intervals stay in the list: a later restarted or re-planned upstream
        /// must not rewrite time the human already cost.
        fn waits(
            node: &NodePath,
            steps: &[DependencyStep],
            asks: &[WaitAsk],
            features: &BTreeMap<NodePath, FeatureFold>,
        ) -> Vec<(UtcTime, Option<UtcTime>)> {
            let mut intervals = Vec::new();
            for ask in asks {
                if &ask.node == node {
                    intervals.push((ask.start, ask.end));
                    continue;
                }
                let Some(feature) = features.get(&ask.node) else {
                    continue;
                };
                for (span_start, span_end) in upstream_spans(steps, &ask.node) {
                    let start = span_start.max(ask.start);
                    let clipped = match (span_end, ask.end) {
                        (Some(end), Some(ask_end)) => Some(end.min(ask_end)),
                        (Some(end), None) => Some(end),
                        (None, Some(ask_end)) => Some(ask_end),
                        (None, None) => None,
                    };
                    if clipped.is_some_and(|end| end.millis() <= start.millis()) {
                        continue;
                    }
                    if satisfied_at(feature, start) {
                        continue;
                    }
                    let end = match (clipped, completion_after(feature, start)) {
                        (Some(end), Some(resolved)) => Some(end.min(resolved)),
                        (Some(end), None) => Some(end),
                        (None, Some(resolved)) => Some(resolved),
                        (None, None) => None,
                    };
                    if end.is_some_and(|end| end.millis() <= start.millis()) {
                        continue;
                    }
                    intervals.push((start, end));
                }
            }
            intervals
        }
    }

    fn time(minute: usize) -> UtcTime {
        UtcTime::parse("2026-01-01T00:00:00.000Z")
            .expect("base")
            .add_seconds(i64::try_from(minute * 60).expect("small"))
    }

    fn node(index: usize) -> NodePath {
        NodePath::parse(&format!("n{index}")).expect("node")
    }

    fn event(index: usize) -> EventId {
        EventId::parse(&format!("{index:026}")).expect("event")
    }

    fn attempt(id: usize, started: usize, finished: Option<usize>, dropped: bool) -> AttemptFold {
        let eta = EtaRange {
            min_s: Seconds::new(60).expect("eta"),
            max_s: Seconds::new(120).expect("eta"),
        };
        AttemptFold {
            id: event(id),
            session: SessionId::parse("00000000000000000000000101").expect("session"),
            started_by: SessionId::parse("00000000000000000000000101").expect("session"),
            sessions: BTreeSet::new(),
            deliveries: Vec::new(),
            harness: Harness::Generic,
            started_at: time(started),
            order: id,
            start_eta: eta,
            eta,
            eta_ts: time(started),
            eta_order: id,
            status: if dropped {
                WorkStatus::Dropped
            } else if finished.is_some() {
                WorkStatus::Done
            } else {
                WorkStatus::Started
            },
            status_event: None,
            status_ts: None,
            first_completion: finished.map(|minute| (id, time(minute))),
            causes: BTreeSet::new(),
            applied: BTreeSet::new(),
            start_basis: BTreeSet::new(),
            eta_signals: Vec::new(),
            before_drop: None,
            settle: crate::reduce::settle::Settle::default(),
            last_word: (time(started), event(id)),
            words: Vec::new(),
        }
    }

    /// Closed intervals as their union (disjoint, sorted) and the earliest
    /// open start: the only facts union measures and pause checks read.
    fn normalize(intervals: &[Interval]) -> (Vec<(i64, i64)>, Option<i64>) {
        let mut closed: Vec<(i64, i64)> = intervals
            .iter()
            .filter_map(|(start, end)| end.map(|end| (start.millis(), end.millis())))
            .filter(|(start, end)| end > start)
            .collect();
        closed.sort();
        let mut merged: Vec<(i64, i64)> = Vec::new();
        for (start, end) in closed {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        let open = intervals
            .iter()
            .filter(|(_, end)| end.is_none())
            .map(|(start, _)| start.millis())
            .min();
        (merged, open)
    }

    fn assert_same(features: &BTreeMap<NodePath, FeatureFold>, asks: &[WaitAsk], context: &str) {
        let fast = by_node(features, asks);
        let slow = reference::by_node(features, asks);
        assert_eq!(fast.len(), slow.len(), "{context}");
        for (node, expected) in &slow {
            let actual = &fast[node];
            assert_eq!(
                normalize(actual),
                normalize(expected),
                "{context}: {node:?}"
            );
            for (anchor, end, as_of) in [(0, 400, 400), (5, 30, 20), (12, 200, 9), (0, 3, 60)] {
                assert_eq!(
                    union_millis(actual, time(anchor), time(end), time(as_of)),
                    union_millis(expected, time(anchor), time(end), time(as_of)),
                    "{context}: {node:?} window {anchor}-{end} at {as_of}"
                );
            }
        }
    }

    struct Random(u64);

    impl Random {
        fn below(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            usize::try_from(self.0 % bound as u64).expect("small")
        }
    }

    /// Random plans with re-declarations, removals, self and unknown edges,
    /// cycles, skewed transition clocks, restarts, drops and open or closed
    /// asks on known and unknown features.
    #[test]
    fn incremental_waits_match_the_full_replay() {
        let mut random = Random(0x2545_f491_4f6c_dd1d);
        for case in 0..3000 {
            random_case(&mut random, 7, 12, &format!("small case {case}"));
        }
        for case in 0..300 {
            random_case(&mut random, 20, 40, &format!("large case {case}"));
        }
    }

    fn random_case(random: &mut Random, max_nodes: usize, max_transitions: usize, context: &str) {
        let count = 1 + random.below(max_nodes);
        let mut features: BTreeMap<NodePath, FeatureFold> = BTreeMap::new();
        for index in 0..count {
            features.insert(node(index), FeatureFold::new(node(index)));
        }
        let transitions = random.below(max_transitions);
        let mut orders: Vec<usize> = (0..transitions).collect();
        for index in (1..orders.len()).rev() {
            orders.swap(index, random.below(index + 1));
        }
        for order in orders {
            // Now and then two changes share one transition.
            let order = if order > 0 && random.below(8) == 0 {
                order - 1
            } else {
                order
            };
            let target = node(random.below(count));
            let mut depends_on = Vec::new();
            for _ in 0..random.below(4) {
                // `count` names a feature that is never declared.
                depends_on.push(node(random.below(count + 1)));
            }
            let feature = features.get_mut(&target).expect("feature");
            feature.depends_on_history.push(DependencyChange {
                order,
                // Not monotonic in `order`: the replay forces it.
                ts: time(random.below(40)),
                depends_on,
            });
        }
        let mut next_id = 1000;
        for feature in features.values_mut() {
            for _ in 0..random.below(3) {
                next_id += 1;
                let started = random.below(40);
                let finished = (random.below(3) > 0).then(|| started + random.below(15));
                let dropped = random.below(6) == 0;
                feature
                    .attempts
                    .insert(event(next_id), attempt(next_id, started, finished, dropped));
            }
        }
        let mut asks = Vec::new();
        for index in 0..random.below(5) {
            let start = random.below(45);
            let end = match random.below(4) {
                0 => None,
                1 => Some(time(start)),
                _ => Some(time(start + random.below(25))),
            };
            asks.push(WaitAsk {
                node: node(random.below(count + 1)),
                source: event(index + 1),
                start: time(start),
                end,
            });
        }
        assert_same(&features, &asks, context);
    }

    /// A long-open ask on the head of a chain: the hold follows edges as they
    /// are added and removed, and a cycle keeps only the edges accepted first.
    #[test]
    fn holds_follow_dependency_adds_removes_and_cycles() {
        let mut features: BTreeMap<NodePath, FeatureFold> = BTreeMap::new();
        for index in 0..4 {
            features.insert(node(index), FeatureFold::new(node(index)));
        }
        let history = [
            (1, 1, vec![node(0)]),           // n1 <- n0
            (2, 2, vec![node(1)]),           // n2 <- n1
            (1, 10, vec![]),                 // n1 drops n0: n1 and n2 stop waiting
            (1, 20, vec![node(0)]),          // back again
            (3, 30, vec![node(2)]),          // n3 <- n2
            (1, 40, vec![node(0), node(3)]), // n1 <- n3 closes a cycle
        ];
        for (order, (target, minute, depends_on)) in history.into_iter().enumerate() {
            features
                .get_mut(&node(target))
                .expect("feature")
                .depends_on_history
                .push(DependencyChange {
                    order,
                    ts: time(minute),
                    depends_on,
                });
        }
        features
            .get_mut(&node(0))
            .expect("feature")
            .attempts
            .insert(event(900), attempt(900, 0, None, false));
        let asks = [WaitAsk {
            node: node(0),
            source: event(1),
            start: time(0),
            end: None,
        }];
        assert_same(&features, &asks, "chain");
        let waits = by_node(&features, &asks);
        // n1 is held from its first edge until the removal, then from the
        // re-add on; touching spans are merged and the last one stays open.
        assert_eq!(
            waits[&node(1)],
            vec![
                (time(1), Some(time(10))),
                (time(20), Some(time(40))),
                (time(40), None)
            ]
        );
        assert_eq!(waits[&node(0)], vec![(time(0), None)]);
        assert_eq!(
            waits[&node(2)],
            vec![
                (time(2), Some(time(10))),
                (time(20), Some(time(40))),
                (time(40), None)
            ]
        );
        // n3 joins at minute 30. At 40 `n1 <- n3` is accepted first (n1 sorts
        // before n3), so `n3 <- n2` is the rejected edge and n3 stops waiting.
        assert_eq!(waits[&node(3)], vec![(time(30), Some(time(40)))]);
    }
}
