import Foundation

enum LockError: Error {
    case heldBy(agentId: String, expiresIn: Int)
}

struct ResourceLock: Codable {
    let name: String
    let agentId: String
    let expires: Date

    var isExpired: Bool { expires < Date() }
    var expiresInSeconds: Int { max(0, Int(expires.timeIntervalSinceNow)) }
}

final class ResourceLockStore {
    private let queue = DispatchQueue(label: "com.agentpeek.locks")
    private var locks: [String: ResourceLock] = [:]
    private let defaultsKey = "agentpeek.locks"

    init() { load() }

    func lock(name: String, agentId: String, ttlMinutes: Int) throws {
        try queue.sync {
            pruneExpired()
            if let existing = locks[name], !existing.isExpired {
                throw LockError.heldBy(agentId: existing.agentId, expiresIn: existing.expiresInSeconds)
            }
            locks[name] = ResourceLock(
                name: name,
                agentId: agentId,
                expires: Date().addingTimeInterval(Double(ttlMinutes) * 60)
            )
            save()
        }
    }

    func unlock(name: String, agentId: String) {
        queue.sync {
            guard let existing = locks[name], existing.agentId == agentId else { return }
            locks.removeValue(forKey: name)
            save()
        }
    }

    func renew(name: String, agentId: String, ttlMinutes: Int) throws {
        try queue.sync {
            guard let existing = locks[name], existing.agentId == agentId, !existing.isExpired else {
                throw LockError.heldBy(agentId: locks[name]?.agentId ?? "none", expiresIn: 0)
            }
            locks[name] = ResourceLock(
                name: name,
                agentId: agentId,
                expires: Date().addingTimeInterval(Double(ttlMinutes) * 60)
            )
            save()
        }
    }

    func allActiveLocks() -> [ResourceLock] {
        queue.sync {
            pruneExpired()
            return Array(locks.values)
        }
    }

    private func pruneExpired() {
        locks = locks.filter { !$0.value.isExpired }
        save()
    }

    private func save() {
        let data = try? JSONEncoder().encode(locks)
        UserDefaults.standard.set(data, forKey: defaultsKey)
    }

    private func load() {
        guard let data = UserDefaults.standard.data(forKey: defaultsKey),
              let decoded = try? JSONDecoder().decode([String: ResourceLock].self, from: data) else { return }
        locks = decoded
    }
}
