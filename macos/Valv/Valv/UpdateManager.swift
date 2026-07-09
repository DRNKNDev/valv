import Combine
import DaemonKit
import Foundation
import Sparkle

final class UpdateManager: NSObject, ObservableObject, SPUUpdaterDelegate {
    static let shared = UpdateManager()

    @Published private(set) var isChecking = false
    @Published private(set) var updateAvailable = false
    @Published private(set) var verificationFailed = false
    @Published private(set) var updateRequired = false

    private var lastKnownUpdateRequired = false
    private var cancellables = Set<AnyCancellable>()

    private let immediateCheckOperation: (() -> Void)?
    private let manualCheckOperation: (() -> Void)?

    private lazy var controller: SPUStandardUpdaterController = SPUStandardUpdaterController(
        startingUpdater: true,
        updaterDelegate: self,
        userDriverDelegate: nil
    )

    init(
        updateRequiredPublisher: AnyPublisher<Bool, Never>? = nil,
        immediateCheckOperation: (() -> Void)? = nil,
        manualCheckOperation: (() -> Void)? = nil
    ) {
        self.immediateCheckOperation = immediateCheckOperation
        self.manualCheckOperation = manualCheckOperation
        super.init()

        let publisher = updateRequiredPublisher
            ?? DaemonStore.shared.$status.map { $0?.updateRequired ?? false }.eraseToAnyPublisher()
        publisher
            .sink { [weak self] in self?.handleUpdateRequiredTransition(to: $0) }
            .store(in: &cancellables)
    }


    func handleCheckStarted() {
        isChecking = true
    }

    func handleValidUpdateFound(version: String) {
        isChecking = false
        updateAvailable = true
    }

    func handleNoUpdateFound() {
        isChecking = false
        updateAvailable = false
    }

    func handleAbort(error: Error) {
        isChecking = false
        verificationFailed = SparkleUpdateError.isSignatureVerificationFailure(error)
    }


    func handleUpdateRequiredTransition(to newValue: Bool) {
        defer { lastKnownUpdateRequired = newValue }
        updateRequired = newValue
        guard newValue, !lastKnownUpdateRequired else { return }
        triggerImmediateBackgroundCheck()
    }

    func triggerImmediateBackgroundCheck() {
        handleCheckStarted()
        if let immediateCheckOperation {
            immediateCheckOperation()
            return
        }
        controller.updater.checkForUpdateInformation()
    }


    func checkForUpdates() {
        handleCheckStarted()
        if let manualCheckOperation {
            manualCheckOperation()
            return
        }
        controller.checkForUpdates(nil)
    }


    func updater(_ updater: SPUUpdater, didFindValidUpdate item: SUAppcastItem) {
        handleValidUpdateFound(version: item.displayVersionString)
    }

    func updaterDidNotFindUpdate(_ updater: SPUUpdater) {
        handleNoUpdateFound()
    }

    func updater(_ updater: SPUUpdater, didAbortWithError error: Error) {
        handleAbort(error: error)
    }
}
