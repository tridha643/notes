// Trace steps: parsing an uploaded trace asset, and folding a session's trace into
// the timeline between posts. Ports sideshow's TraceSurface.parseTrace and
// SessionTimeline (buildGaps, groupTurns, the "··· N steps ···" fold).

import Foundation

/// A JSON array of steps, a single step object, or JSONL (one per line). Entries
/// without a string `label` are dropped; a bare string becomes a label-only step,
/// and a JSONL line that isn't JSON becomes a step labelled with the line.
public func parseTrace(_ text: String) -> [TraceStep] {
    let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !trimmed.isEmpty else { return [] }
    func step(_ v: JSONValue) -> TraceStep? {
        if case .string(let s) = v { return TraceStep(label: s) }
        guard case .string(let label)? = v["label"] else { return nil }
        func str(_ k: String) -> String? { if case .string(let s)? = v[k] { s } else { nil } }
        return TraceStep(label: label, kind: str("kind"), detail: str("detail"), ts: str("ts"))
    }
    if let whole = try? JSONValue.parse(Data(trimmed.utf8)) {
        if case .array(let items) = whole { return items.compactMap(step) }
        return [whole].compactMap(step)
    }
    return trimmed.split(separator: "\n").compactMap { raw in
        let line = raw.trimmingCharacters(in: .whitespaces)
        guard !line.isEmpty else { return nil }
        guard let v = try? JSONValue.parse(Data(line.utf8)) else { return TraceStep(label: line) }
        return step(v)
    }
}

/// Unix seconds from an ISO 8601 `ts`, with or without fractional seconds.
public func traceTime(_ ts: String?) -> Double? {
    guard let ts else { return nil }
    if let d = try? Date(ts, strategy: .iso8601) { return d.timeIntervalSince1970 }
    if let d = try? Date(ts, strategy: Date.ISO8601FormatStyle(includingFractionalSeconds: true)) { return d.timeIntervalSince1970 }
    return nil
}

/// One visible line of the session trace in the timeline.
public enum TimelineEntry: Hashable, Sendable {
    /// `steps[index]`; `prompt` rows get the rail marker, `say` ones read as responses.
    case step(index: Int, style: Style)
    /// The folded middle of a turn: `count` steps, keyed by its first step's index.
    case fold(first: Int, count: Int, open: Bool)

    public enum Style: Hashable, Sendable { case prompt, response, command }
}

/// Splits the session trace into the gaps before each post (`posts` are creation
/// times, ascending) plus a trailing gap, then lays each gap out as turns: the
/// prompt, the first `say`, a fold over everything between, the last `say`.
/// `open` holds the `first` index of expanded folds. Result has posts.count + 1 gaps.
public func timelineGaps(steps: [TraceStep], posts: [Double], open: Set<Int>) -> [[TimelineEntry]] {
    var gaps = Array(repeating: [Int](), count: posts.count + 1)
    for (i, s) in steps.enumerated() {
        var gap = posts.count
        if let t = traceTime(s.ts), let found = posts.firstIndex(where: { $0 >= t }) { gap = found }
        gaps[gap].append(i)
    }
    return gaps.map { indices in
        // Turns start at each prompt.
        var turns: [(prompt: Int?, events: [Int])] = []
        for i in indices {
            if steps[i].kind == "prompt" { turns.append((i, [])) }
            else if turns.isEmpty { turns.append((nil, [i])) }
            else { turns[turns.count - 1].events.append(i) }
        }
        var out: [TimelineEntry] = []
        for turn in turns {
            if let p = turn.prompt { out.append(.step(index: p, style: .prompt)) }
            let says = turn.events.filter { steps[$0].kind == "say" }
            let intent = says.first
            let outcome = says.count > 1 ? says.last : nil
            if let intent { out.append(.step(index: intent, style: .response)) }
            let middle = turn.events.filter { $0 != intent && $0 != outcome }
            if let first = middle.first {
                let isOpen = open.contains(first)
                out.append(.fold(first: first, count: middle.count, open: isOpen))
                if isOpen {
                    out += middle.map { .step(index: $0, style: steps[$0].kind == "say" ? .response : .command) }
                }
            }
            if let outcome { out.append(.step(index: outcome, style: .response)) }
        }
        return out
    }
}
