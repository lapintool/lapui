# Windows runtime prerequisite

Lapui currently imports `VCRUNTIME140.dll` and the Visual C++ 14.x Universal CRT API sets. On a clean Windows x64 machine, install the latest supported **Visual C++ Redistributable x64** before starting `lapui.exe`:

<https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist>

The current 14.x redistributable is binary-compatible with applications built by Visual Studio 2015 and later. If a compatible x64 runtime is already installed, no additional setup is needed.

This preview package does not include or modify Microsoft runtime files. Microsoft limits redistribution of the runtime package and individual binaries to licensed Visual Studio users, subject to the applicable license terms. See Microsoft's [redistribution guidance](https://learn.microsoft.com/en-us/cpp/windows/redistributing-visual-cpp-files) before creating a package that bundles the redistributable.
