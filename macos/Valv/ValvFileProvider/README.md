# File Provider Extension

`ValvFileProvider` enumerates, materializes, and mutates items against the local `valvd` daemon over loopback TCP, using the shared [`DaemonKit`](../../DaemonKit) client.

- `FileProviderExtension.swift`: the `NSFileProviderReplicatedExtension` entry point.
- `FileProviderEnumerator.swift`: directory and change enumeration.
- `FileProviderItem.swift`: item metadata.

It builds as part of the `Valv` Xcode project. See [`../README.md`](../README.md) for the host app and the other targets, and [`../../../contracts/ipc/README.md`](../../../contracts/ipc/README.md) for the `fileprovider` control protocol it consumes.
