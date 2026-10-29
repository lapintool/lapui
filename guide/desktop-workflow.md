# Root pnpm desktop workflow

Run these commands from the Lapui repository root on Windows:

| Command | Result |
| --- | --- |
| pnpm dev | Build/watch the Lapstyle UI frontend, compile the Rust backend, and open the native Lapui window. |
| pnpm release | Type-check/build the frontend and build the Rust release executable, then assemble a portable application directory. |
| pnpm build | Perform the release workflow and generate a Windows x64 NSIS setup executable. |
| pnpm doctor | Check the configured frontend, Python, Rust toolchain, and installer compiler. |
| pnpm setup:installer | Install the supported NSIS compiler through winget if it is missing. |

These are development and packaging commands for the existing experimental runtime. They do not change its documented frontend compatibility.

## Prerequisites and configuration

Use Node.js 24 or newer, pnpm 11.8.0, Python 3, Rust 1.99.0, and the Windows MSVC build tools. The root package has no npm dependencies; run pnpm install to validate its lockfile. The external frontend checkout needs its own dependencies installed.

The tracked lapui.config.json selects:

- frontendDirectory: ../../Web/lapstyle-ui, relative to this repository.
- rustToolchain: 1.99.0.
- renderer: gpu for development. The 4K Lapstyle UI workload is expensive to rasterize on the UI thread with the CPU backend; GPU drawing reduces that part of the frame cost.

On the development machine, this resolves to C:\Dev\Projects\Web\lapstyle-ui. Other checkout locations can use a command-line override:

    pnpm dev --frontend C:\Other\lapstyle-ui
    pnpm dev --renderer cpu

The corresponding environment variables are LAPUI_FRONTEND_DIR, LAPUI_RUST_TOOLCHAIN, and LAPUI_RENDERER. Command-line arguments take precedence over environment variables, followed by the config file. Use pnpm --dir <frontend-directory> install if the frontend's dependencies are missing. If the GPU backend cannot initialize on a machine, explicitly select cpu; automatic GPU fallback is not implemented. Direct cargo/executable launches still default to CPU when the software-renderer feature is enabled; pass --renderer gpu for the same backend as pnpm dev.

## Development

    pnpm dev

Frontend development uses Vite's watched local build. Lapui loads the generated local files rather than Vite's browser development-server protocol. After a successful frontend build, the parent process requests a complete document reload through Lapui's local control endpoint. Reload is requested only after the bundle and plugin assets have finished writing. Frontend drafts and focus reset on reload.

The initial launch runs frontend type checking once. Subsequent watched builds report bundling errors; release/build run the full type check again. Rust sources under src, the embedded JavaScript bridge under ui, vendored Taffy sources, Cargo.toml, Cargo.lock, and build.rs are watched. Saving these changes stops the current window, compiles again, and opens the rebuilt program; compile errors remain in the terminal until a later save retries the build.

The terminal prints LAPUI_CONTROL=127.0.0.1:<port>. That address changes when the Rust process restarts. Existing diagnostics, controls, screenshot, and reload commands remain available. Close the UI window or press Ctrl+C in the development terminal to stop the backend and frontend watcher together.

Frontend build output is isolated under target/desktop/dev/app. It does not overwrite the external frontend checkout's dist directory.

## Release files and installer

    pnpm release
    pnpm build

On Windows x64 the outputs are:

    dist/
      lapui-windows-x64/
        lapui.exe
        app/index.html
        app/assets/...
        BUILD-MANIFEST.json
        LICENSE-MIT
        LICENSE-APACHE
        THIRD_PARTY.md
        licenses/...
        prerequisites/README.md
      Lapui-0.1.0-windows-x64-setup.exe

The setup file is generated only by pnpm build. The release executable automatically loads app/index.html beside itself, regardless of the terminal's working directory. Copy the complete portable directory when moving the program; the executable alone does not contain the Lapstyle UI bundle. The installed application does not need the original frontend checkout, Node, pnpm, Python, or Rust.

An explicit --html, --js, or --demo still selects its requested source. Without adjacent packaged resources, the runtime continues to open the embedded counter example. Both CPU and GPU are compiled into the default package; use lapui.exe --renderer gpu to select GPU at runtime.

BUILD-MANIFEST.json records the backend/frontend commits and dirty state, version, toolchain, platform, and SHA-256 hashes of packaged files. Set the same version in package.json and Cargo.toml when changing the application version.

The installer uses NSIS 3.13, installs for the current user under LocalAppData\Programs\Lapui by default, and creates Start Menu shortcuts and a Windows uninstall entry. Uninstall removes the enumerated package files; unrelated files are left in place. If NSIS is absent, pnpm build invokes winget to install it. NSIS_EXE can point to an existing makensis.exe instead. Building this installer currently requires Windows x64.

The package includes the existing Microsoft VC++ runtime prerequisite instructions. This build command does not silently install or bundle the Microsoft runtime on destination machines. Clean-machine acceptance and broader rendering/interaction acceptance remain separate from producing an installer.

The older Windows preview staging scripts are tied to a historical executable hash. They are not used by the root pnpm workflow and should not be used to validate a newly built executable.
