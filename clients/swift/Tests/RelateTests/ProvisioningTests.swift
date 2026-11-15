import Foundation
import Testing

@testable import Relate

@Suite struct ProvisioningTests {
    /// A checkpointed store of `books` books at `url`, as an app bundle
    /// would carry it. Creates the directory it sits in.
    private func makeSeed(at url: URL, books: Int) throws {
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let db = try Database(url: url)
        try db.connect().run("UNWIND range(1, \(books)) AS i CREATE (:Book {i: i})")
        try db.checkpoint()
    }

    private func bookCount(at url: URL) throws -> Int {
        try Database(url: url).connect().query("MATCH (b:Book) RETURN count(b)").scalar(as: Int.self)
    }

    private func permissions(_ url: URL) throws -> Int {
        try #require(FileManager.default.attributesOfItem(atPath: url.path)[.posixPermissions] as? Int)
    }

    /// The leftover of a provision of `target` that was started `age`
    /// seconds ago and never finished.
    private func leaveCopy(of target: URL, startedAgo age: TimeInterval) throws -> URL {
        let leftover = ProvisioningCopy(target: target).freshURL(startedAt: Date(timeIntervalSinceNow: -age))
        try Data("partial".utf8).write(to: leftover)
        return leftover
    }

    private func names(in directory: URL) throws -> Set<String> {
        try Set(FileManager.default.contentsOfDirectory(atPath: directory.path))
    }

    @Test func aBundledSingleFileStoreIsCopiedAndOpensWritable() async throws {
        try await withTemporaryDirectory { scratch in
            let seed = scratch.appendingPathComponent("bundle/books.rl")
            try makeSeed(at: seed, books: 3)
            // A bundle resource is read-only.
            try FileManager.default.setAttributes([.posixPermissions: 0o444], ofItemAtPath: seed.path)
            let target = scratch.appendingPathComponent("support/nested/library.rl")

            #expect(try Database.provision(at: target, from: seed))
            #expect(try permissions(target) == 0o600)
            #expect(try bookCount(at: target) == 3)
            try Database(url: target).connect().query("CREATE (:Book {i: 4})")
            // The seed is untouched and no temporary copy remains.
            #expect(try permissions(seed) == 0o444)
            #expect(try names(in: target.deletingLastPathComponent()) == ["library.rl"])
        }
    }

    @Test func aStoreThatIsAlreadyThereIsLeftAlone() async throws {
        try await withTemporaryDirectory { scratch in
            let seed = scratch.appendingPathComponent("bundle/books.rl")
            try makeSeed(at: seed, books: 3)
            let target = scratch.appendingPathComponent("library.rl")
            #expect(try Database.provision(at: target, from: seed))
            try Database(url: target).connect().query("CREATE (:Book {i: 4})")
            // A second launch finds the store and keeps the user's change.
            #expect(try !Database.provision(at: target, from: seed))
            #expect(try bookCount(at: target) == 4)
        }
    }

    @Test func aDirectoryStoreIsCopiedWithEveryItemOwnerWritable() async throws {
        try await withTemporaryDirectory { scratch in
            let seed = scratch.appendingPathComponent("seed-store", isDirectory: true)
            try makeSeed(at: seed, books: 2)
            // A bundle resource is read-only, all the way down.
            let seedItems = try FileManager.default.subpathsOfDirectory(atPath: seed.path)
            #expect(!seedItems.isEmpty)
            for item in seedItems {
                try FileManager.default.setAttributes(
                    [.posixPermissions: 0o555], ofItemAtPath: seed.appendingPathComponent(item).path)
            }
            let target = scratch.appendingPathComponent("store", isDirectory: true)
            #expect(try Database.provision(at: target, from: seed))
            #expect(try permissions(target) == 0o700)
            for item in try FileManager.default.subpathsOfDirectory(atPath: target.path) {
                let url = target.appendingPathComponent(item)
                #expect(try permissions(url) == (try Database.isDirectory(url) ? 0o700 : 0o600), "\(item)")
            }
            #expect(try bookCount(at: target) == 2)
            try Database(url: target).connect().query("CREATE (:Book {i: 3})")
        }
    }

    @Test func aSeedThatIsASymbolicLinkIsCopiedNotLinked() async throws {
        try await withTemporaryDirectory { scratch in
            let seed = scratch.appendingPathComponent("bundle/books.rl")
            try makeSeed(at: seed, books: 1)
            let link = scratch.appendingPathComponent("link.rl")
            try FileManager.default.createSymbolicLink(at: link, withDestinationURL: seed)
            let target = scratch.appendingPathComponent("library.rl")
            #expect(try Database.provision(at: target, from: link))
            let type = try FileManager.default.attributesOfItem(atPath: target.path)[.type] as? FileAttributeType
            #expect(type == .typeRegular)
            #expect(try bookCount(at: target) == 1)
        }
    }

    // MARK: Leftovers

    @Test func anAbandonedCopyIsRemoved() async throws {
        try await withTemporaryDirectory { scratch in
            let target = scratch.appendingPathComponent("library.rl")
            let seed = scratch.appendingPathComponent("bundle/books.rl")
            try makeSeed(at: seed, books: 1)
            let abandoned = try leaveCopy(of: target, startedAgo: ProvisioningCopy.staleAfter + 60)
            #expect(try Database.provision(at: target, from: seed))
            #expect(try !names(in: scratch).contains(abandoned.lastPathComponent))
        }
    }

    @Test func anAbandonedCopyIsRemovedEvenOnceTheStoreIsInPlace() async throws {
        try await withTemporaryDirectory { scratch in
            let target = scratch.appendingPathComponent("library.rl")
            let seed = scratch.appendingPathComponent("bundle/books.rl")
            try makeSeed(at: seed, books: 1)
            #expect(try Database.provision(at: target, from: seed))
            let abandoned = try leaveCopy(of: target, startedAgo: ProvisioningCopy.staleAfter + 60)
            #expect(try !Database.provision(at: target, from: seed))
            #expect(try !names(in: scratch).contains(abandoned.lastPathComponent))
        }
    }

    @Test func aCopyInProgressIsLeftAloneHoweverOldItsSeed() async throws {
        try await withTemporaryDirectory { scratch in
            let target = scratch.appendingPathComponent("library.rl")
            let seed = scratch.appendingPathComponent("bundle/books.rl")
            try makeSeed(at: seed, books: 1)
            // A copy keeps its seed's dates, so a year-old seed makes a
            // year-old-looking copy the moment it is made.
            try FileManager.default.setAttributes(
                [.modificationDate: Date(timeIntervalSinceNow: -365 * 86_400)], ofItemAtPath: seed.path)
            let inProgress = ProvisioningCopy(target: target).freshURL()
            try FileManager.default.copyItem(at: seed, to: inProgress)
            #expect(try Database.provision(at: target, from: seed))
            #expect(try names(in: scratch).contains(inProgress.lastPathComponent))
        }
    }

    @Test func anotherTargetsCopyIsNotThisOnesToRemove() async throws {
        try await withTemporaryDirectory { scratch in
            let seed = scratch.appendingPathComponent("bundle/books.rl")
            try makeSeed(at: seed, books: 1)
            let old = ProvisioningCopy.staleAfter + 60
            let unrelated = try leaveCopy(of: scratch.appendingPathComponent("other.rl"), startedAgo: old)
            // `library` and `library.rl` share a prefix; their copies are still their own.
            let longerName = try leaveCopy(of: scratch.appendingPathComponent("library.rl"), startedAgo: old)
            #expect(try Database.provision(at: scratch.appendingPathComponent("library"), from: seed))
            #expect(try names(in: scratch).isSuperset(of: [unrelated.lastPathComponent, longerName.lastPathComponent]))
        }
    }

    @Test func aMissingSeedThrowsAndLeavesNothingBehind() async throws {
        try await withTemporaryDirectory { scratch in
            let target = scratch.appendingPathComponent("library.rl")
            let error = try #require(throws: CocoaError.self) {
                try Database.provision(at: target, from: scratch.appendingPathComponent("no-such-seed.rl"))
            }
            #expect(error.code == .fileReadNoSuchFile || error.code == .fileNoSuchFile)
            #expect(try names(in: scratch) == [])
        }
    }

    @Test func aURLThatIsNotALocalFileIsRefusedForEitherEnd() async throws {
        try await withTemporaryDirectory { scratch in
            let local = scratch.appendingPathComponent("library.rl")
            let remote = try #require(URL(string: "https://example.com/books.rl"))
            let elsewhere = try #require(URL(string: "file://fileserver/share/books.rl"))
            let seedRefusal = try invalidParameterMessage { _ = try Database.provision(at: local, from: remote) }
            #expect(seedRefusal.contains("must be a file URL"))
            let targetRefusal = try invalidParameterMessage { _ = try Database.provision(at: elsewhere, from: local) }
            #expect(targetRefusal.contains("must name a local file"))
            #expect(try names(in: scratch) == [])
        }
    }

    @Test func anItemThatCannotBeFoundIsNotTakenForAFile() async throws {
        try await withTemporaryDirectory { scratch in
            let error = try #require(throws: CocoaError.self) {
                try Database.isDirectory(scratch.appendingPathComponent("gone"))
            }
            #expect(error.code == .fileReadNoSuchFile)
            #expect(try Database.isDirectory(scratch))
            let file = scratch.appendingPathComponent("file")
            try Data().write(to: file)
            #expect(try !Database.isDirectory(file))
        }
    }

    // MARK: The default location

    @Test func theDefaultStoreLocationNestsUnderTheBundleIdentifier() async throws {
        try await withTemporaryDirectory { scratch in
            let plain = try Database.storeURL(named: "library.rl", in: scratch, bundleIdentifier: nil)
            #expect(plain == scratch.appendingPathComponent("library.rl"))
            let nested = try Database.storeURL(
                named: "library.rl", in: scratch, bundleIdentifier: "com.example.books")
            #expect(nested.path.hasSuffix("/com.example.books/library.rl"))
            var isDirectory: ObjCBool = false
            #expect(FileManager.default.fileExists(
                atPath: nested.deletingLastPathComponent().path, isDirectory: &isDirectory))
            #expect(isDirectory.boolValue && !FileManager.default.fileExists(atPath: nested.path))
        }
    }

    @Test(arguments: ["", ".", "..", "../escape.rl", "nested/library.rl"])
    func aStoreNameThatIsNotOnePathComponentIsRefused(name: String) async throws {
        try await withTemporaryDirectory { scratch in
            let message = try invalidParameterMessage {
                _ = try Database.storeURL(named: name, in: scratch, bundleIdentifier: nil)
            }
            #expect(message.contains("one path component"))
            // Nothing was created for a refused name.
            #expect(try names(in: scratch) == [])
        }
    }

    @Test func aRefusedStoreNameIsEchoedSanitized() async throws {
        try await withTemporaryDirectory { scratch in
            let bell = String(UnicodeScalar(7))
            let message = try invalidParameterMessage {
                _ = try Database.storeURL(named: "a/b" + bell + "\n", in: scratch, bundleIdentifier: nil)
            }
            #expect(!message.contains(bell) && !message.contains("\n"))
        }
    }
}
