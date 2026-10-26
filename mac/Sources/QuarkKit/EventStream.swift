import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

/// Follows quarkd's event stream (`GET /v1/events`, WebSocket), reconnecting
/// with the last seen `seq` so no event is missed or repeated. Ends when the
/// consuming task is cancelled.
public func quarkEvents(_ client: QuarkClient, from cursor: Int64? = nil) -> AsyncStream<Event> {
    AsyncStream { continuation in
        let task = Task {
            var cursor = cursor
            var delay: UInt64 = 1
            while !Task.isCancelled {
                let socket = client.session.webSocketTask(with: client.eventsURL(cursor: cursor))
                socket.resume()
                do {
                    while !Task.isCancelled {
                        let message = try await socket.receive()
                        delay = 1
                        let data: Data
                        switch message {
                        case let .string(s): data = Data(s.utf8)
                        case let .data(d): data = d
                        @unknown default: continue
                        }
                        guard let event = try? JSONDecoder.quark.decode(Event.self, from: data) else { continue }
                        cursor = event.seq
                        continuation.yield(event)
                    }
                } catch {
                    socket.cancel(with: .goingAway, reason: nil)
                }
                // quarkd is down or restarting: back off up to 30 s.
                try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
                delay = min(delay * 2, 30)
            }
            continuation.finish()
        }
        continuation.onTermination = { _ in task.cancel() }
    }
}
