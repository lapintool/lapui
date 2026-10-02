# Lapui

[English](README.md) · [项目文档](guide/README.md)

Lapui 是一个使用 Rust 编写的桌面 UI 运行时实验。它用 Blitz 渲染 HTML/CSS，以 QuickJS-ng 执行 JavaScript，目标是在不打包 Chromium、也不依赖系统 WebView 的情况下运行本地界面。

**当前状态：临时的、尚未完工的开发预览版。** P1–P3 验收仍在进行，行为和 API 可能变化，本版本不适用于生产环境。已验证单窗口、有限的动态 DOM 与表单、捕获/冒泡事件、本地模块和计时器、语义控件与诊断、异步 Rust 动作、HTTP/WebSocket/SSE，以及可运行的 Vue 3 和 React DOM 示例。它还不是通用浏览器，也不是可用于生产的前端框架运行时。

## 运行原型

在 Windows 上安装 Rust MSVC 工具链后执行：

```powershell
cargo run --release --locked
```

默认使用 CPU 绘制；`--renderer gpu` 可切回原 GPU 路径。`--snapshot target/demo.png` 可无窗口导出当前文档。行为边界与测量见[绘制及图像导出](guide/rendering.md)。

程序会打开计数器窗口，并输出 `LAPUI_CONTROL=127.0.0.1:<port>`。在另一个终端使用输出的地址：

```powershell
.\target\release\lapui.exe client 127.0.0.1:<port> describe
.\target\release\lapui.exe client 127.0.0.1:<port> observe
.\target\release\lapui.exe client 127.0.0.1:<port> increment
.\target\release\lapui.exe client 127.0.0.1:<port> controls
.\target\release\lapui.exe client 127.0.0.1:<port> diagnostics
.\target\release\lapui.exe client 127.0.0.1:<port> screenshot target/current.png
```

窗口按钮和本机客户端调用同一个 Rust 动作及校验逻辑。`describe` 返回协议方法和当前能力标记；`observe` 返回带版本号的状态和动作参数 schema。控制接口仅绑定本机回环地址，尚无认证机制，不能作为生产接口暴露。

截图直接由运行时生成，无需外部桌面截图或 MCP SDK。Rust 宿主可调用
`lapui::snapshot::capture(&mut document)` 获取独立持有像素的图像，再读取
RGBA、编码或保存 PNG；结果包含文档 epoch、物理尺寸和 DPI。图像包含文档
内容，系统 IME 候选窗和标题栏仍需桌面查看。详见[内置截图 API](guide/rendering.md#built-in-screenshot-api)。

已在 Ubuntu 24.04 / WSL 上使用 Rust 1.91 和 `libfontconfig1-dev` 验证 Linux 编译与测试；WSLg 的 X11 路径已通过 React 窗口及 TCP 激活检查；当前环境的 Wayland 启动失败，物理输入及其他 Linux 桌面尚未验收。

运行本地 HTML 页面：`cargo run -- --html examples/vue-demo/index.html`。Vue 示例的构建和测试方式见[示例说明](examples/vue-demo/README.md)和[入门指南](guide/getting-started.md#run-the-vue-3-example)。

本地 ES module 已支持静态/动态导入、重导出和顶层 await，模块限定在应用目录内。无须 Node 或构建步骤的[模块示例](examples/modules-demo/README.md)：`cargo run --release --locked -- --html examples/modules-demo/index.html`。包名导入仍需构建工具处理，详见[模块指南](guide/modules.md)。

[React DOM 示例](examples/react-demo/README.md)已验证受控输入、捕获/冒泡事件委托、effect、列表与条件内容，以及异步 Rust 动作。启动命令：`cargo run --release --locked -- --html examples/react-demo/index.html`。

本地页面加 `--watch` 可在编辑或重新构建 bundle 后完整重载。`lapui client <地址> reload` 手动重载，`reload-status` 查询最近一次尝试。Rust 应用状态保留，前端草稿与文档引用重置，见[开发及重载指南](guide/development.md)。

[表单示例](guide/forms.md)包含复选框、独立单选组、标签激活、禁用 fieldset、只读输入和键盘交互。无需 Node 或构建步骤：`cargo run --release --locked -- --html examples/forms-demo/index.html --watch`。

[本地表单提交示例](examples/form-submit-demo/README.md)支持提交/重置、字符串 FormData 和基础约束校验，并向 AI 控件接口提供相同的校验状态：`cargo run --release --locked -- --html examples/form-submit-demo/index.html --watch`。范围与限制见[表单文档](guide/forms.md)。

[变更订阅示例](examples/changes-demo/README.md)让界面和本机客户端共享 Rust 状态：`cargo run --release --locked -- --html examples/changes-demo/index.html`。支持暂停/恢复、带游标的有界历史与丢失后的快照恢复，详见[订阅指南](guide/changes.md)。

运行包含中文搜索、文件详情、共享改名动作、对象版本冲突、扫描进度与取消的内存工具示例：

```powershell
cargo run --release --locked -- --demo files
```

示例不会修改磁盘文件。自定义 Rust 后端、操作 ID 和有界追踪见[宿主动作指南](guide/host-actions.md)。

真实目录工具使用 `lapui --demo local-files --directory <路径>`。它只索引顶层
普通文件的名称和大小，不读取内容。界面和 MCP 共用查询、刷新及元数据动作；
备注仅保存在当前进程中，退出后清除，不改名或删除磁盘文件。

AI 客户端可通过 `actions.list` 分页发现动作、`actions.describe` 按需读取 schema、`actions.check` 查询业务阻塞原因。Rust 作用域支持临时动作随所有者或文档关闭而注销，见[动作发现与生命周期](guide/action-discovery.md)。

动画与布局测量示例不需要构建步骤：`cargo run --release --locked -- --html examples/animation-demo/index.html`。支持 requestAnimationFrame、取消、暂停/继续及 CSS 像素边界测量；具体边界见[任务调度](guide/scheduling.md)与[布局测量](guide/geometry.md)。可滚动列表示例：`cargo run --release --locked -- --html examples/scroll-demo/index.html`。

[Floating UI 弹层示例](examples/floating-demo/README.md)使用真实 DOM 定位库及原生计算样式、布局几何：`cargo run --release --locked -- --html examples/floating-demo/index.html`。已验证选定的 offset、flip、shift 场景；弹层打开时，库自身的 autoUpdate 会跟随窗口和锚点尺寸变化。支持范围见[计算样式](guide/computed-styles.md)。

## 项目文档

公开文档使用 [mdBook](https://rust-lang.github.io/mdBook/) 编写。安装 `mdbook` 后，在仓库根目录运行 `mdbook build` 构建，或运行 `mdbook serve` 本地预览。生成的 `book/` 目录已被 Git 忽略。

参见[入门](guide/getting-started.md)、[架构](guide/architecture.md)、[结构化交互](guide/interaction.md)、[兼容性矩阵](guide/compatibility.md)和[当前限制](guide/limitations.md)。

## 许可证

Lapui 原创代码和文档可由使用者任选 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 许可证。依赖项保留各自许可证，见[第三方说明](THIRD_PARTY.md)。
