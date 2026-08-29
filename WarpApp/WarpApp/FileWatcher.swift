import Foundation
import Combine
import CoreServices

class FileWatcher: ObservableObject {
    private var stream: FSEventStreamRef?
    private let paths: [String]
    var onChange: (() -> Void)?

    init(paths: [String]) {
        self.paths = paths
    }

    deinit {
        stop()
    }

    func start() {
        guard stream == nil else { return }

        let cfPaths = paths as CFArray
        var context = FSEventStreamContext()
        context.info = Unmanaged.passUnretained(self).toOpaque()

        let callback: FSEventStreamCallback = { _, clientCallBackInfo, _, _, _, _ in
            guard let info = clientCallBackInfo else { return }
            let watcher = Unmanaged<FileWatcher>.fromOpaque(info).takeUnretainedValue()
            DispatchQueue.main.async {
                watcher.onChange?()
            }
        }

        stream = FSEventStreamCreate(
            nil,
            callback,
            &context,
            cfPaths,
            FSEventStreamEventId(kFSEventStreamEventIdSinceNow),
            1.0,  // 1-second latency debounce
            UInt32(kFSEventStreamCreateFlagUseCFTypes | kFSEventStreamCreateFlagFileEvents)
        )

        if let stream = stream {
            FSEventStreamSetDispatchQueue(stream, DispatchQueue.main)
            FSEventStreamStart(stream)
        }
    }

    func stop() {
        if let stream = stream {
            FSEventStreamStop(stream)
            // When using FSEventStreamSetDispatchQueue, unset the queue before invalidating
            FSEventStreamSetDispatchQueue(stream, nil)
            FSEventStreamInvalidate(stream)
            FSEventStreamRelease(stream)
        }
        stream = nil
    }
}
