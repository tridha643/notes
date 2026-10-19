import Foundation
import NotesCore

let client = DaemonClient()

/// Requests run in order on one serial queue (a save lands before the next read)
/// and their results come back on the main thread.
private let requests = DispatchQueue(label: "dev.notes.app.requests", qos: .userInitiated)

func daemon<T: Sendable>(
    _ work: @escaping @Sendable (DaemonClient) throws -> T,
    then: @escaping @MainActor @Sendable (Result<T, Error>) -> Void
) {
    requests.async {
        let result = Result { try work(client) }
        onMain { then(result) }
    }
}

func onMain(_ body: @escaping @MainActor @Sendable () -> Void) {
    DispatchQueue.main.async { MainActor.assumeIsolated(body) }
}

extension Error {
    var daemonCode: ErrorCode? {
        switch self as? DaemonError {
        case .daemon(let body): body.code
        case .unreachable: .daemonUnreachable
        default: nil
        }
    }

    var message: String {
        switch self as? DaemonError {
        case .daemon(let body): body.message
        case .unreachable(let why): "notesd isn't reachable (\(why))"
        case .protocolError(let why): why
        case nil: localizedDescription
        }
    }
}
