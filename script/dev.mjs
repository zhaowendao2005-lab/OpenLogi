#!/usr/bin/env node
//
// OpenLogi 本地开发一键启动器。
//
// 构建 → 起 agent（服务）→ 等 IPC 端点就绪 → 起 GUI，两个进程的 stdout/stderr
// 都实时、带 [agent] / [gui] 前缀汇到同一个终端；GUI 退出时收掉 agent。
//
//   npm run dev            debug 构建，dev profile，日志 debug
//   npm run dev:mock       无硬件：agent 换成 openlogi-agent-mock
//   npm run dev:release    只生成 release 产物，不启动任何进程
//   npm run dev:gui        只起 GUI（agent 由 GUI 自己按同目录兄弟文件拉起）
//
// 直接调用：node script/dev.mjs [--release] [--mock] [--no-agent] [--no-build]
//                              [--profile dev] [--build-only] [--log <filter>]
//                              [--log-file <path>] [--timeout <sec>]
//
// 本文件重复了 Rust 侧的两处决定，改动时以 owner 为准（见 AGENTS.md
// "Single source of truth"）：
//   * IPC 端点名      crates/openlogi-ipc/src/transport.rs
//   * profile 目录名  crates/openlogi-core/src/paths.rs（runtime_dir_for / config_dir_for）
// 最低 Rust 版本不重复：从 Cargo.toml 的 rust-version 现读。

import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const IS_WINDOWS = process.platform === "win32";

// 与 transport.rs 同一个名字。Windows 的命名管道不在文件系统中，必须
// 显式把 profile 编进名称才能隔离 dev/prod。
const PIPE_NAME = { dev: "openlogi-agent-dev.sock", prod: "openlogi-agent.sock" };

// 与 paths.rs 的 APP_DIR / DEV_APP_DIR 一致。
const APP_DIR = { dev: "openlogi-dev", prod: "openlogi" };

const HELP = `OpenLogi 开发启动器

用法：node script/dev.mjs [选项]

  --release          用 release 构建（默认 debug）
  --mock             起 openlogi-agent-mock 代替真 agent（无硬件可跑 GUI）
  --no-agent         不起 agent，让 GUI 自己拉起（日志改由 agent 自己的日志文件承载）
  --no-build         跳过构建，直接跑现有二进制
  --profile <p>      dev | prod，默认 dev（隔离 config/state/锁，不碰已安装的正式版）
  --log <filter>     tracing filter，默认 info（调试时用 debug）
  --log-file <path>  额外把合并后的日志写到文件
  --timeout <sec>    等 agent IPC 就绪的秒数，默认 20
  -h, --help         显示本帮助
`;

const COLOR = process.stdout.isTTY === true && !process.env.NO_COLOR;
const TAG_COLOR = { dev: "90", build: "34", agent: "36", gui: "35", warn: "33", error: "31" };

function paint(color, text) {
  return COLOR ? `\u001B[${color}m${text}\u001B[0m` : text;
}

let logStream = null;

function emit(text) {
  process.stdout.write(`${text}\n`);
  if (logStream !== null) logStream.write(`${text}\n`);
}

function say(tag, message) {
  emit(`${paint(TAG_COLOR[tag], `[${tag}]`)} ${message}`);
}

function die(message) {
  say("error", message);
  process.exit(1);
}

function parseArgs(argv) {
  const opts = {
    release: false,
    mock: false,
    agent: true,
    build: true,
    buildOnly: false,
    profile: "dev",
    log: "info",
    logFile: null,
    timeoutMs: 20_000,
  };

  // `--key=value` 和 `--key value` 两种写法都收。
  const args = argv.flatMap((arg) => {
    const eq = /^(--[a-z-]+)=(.*)$/.exec(arg);
    return eq === null ? [arg] : [eq[1], eq[2]];
  });

  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    const value = () => {
      const next = args[i + 1];
      if (next === undefined) die(`参数 ${arg} 缺少取值`);
      i += 1;
      return next;
    };

    switch (arg) {
      case "--release":
        opts.release = true;
        break;
      case "--mock":
        opts.mock = true;
        break;
      case "--no-agent":
        opts.agent = false;
        break;
      case "--no-build":
        opts.build = false;
        break;
      case "--build-only":
        opts.buildOnly = true;
        break;
      case "--profile":
        opts.profile = value();
        break;
      case "--log":
        opts.log = value();
        break;
      case "--log-file":
        opts.logFile = path.resolve(value());
        break;
      case "--timeout":
        opts.timeoutMs = Number(value()) * 1000;
        break;
      case "-h":
      case "--help":
        process.stdout.write(HELP);
        process.exit(0);
        break;
      default:
        die(`未知参数 ${arg}（--help 查看用法）`);
    }
  }

  if (opts.profile !== "dev") {
    die("dev 启动器只允许 dev profile；生产环境请使用 pnpm dev:release 生成产物后手动覆盖安装目录");
  }
  if (!Number.isFinite(opts.timeoutMs) || opts.timeoutMs <= 0) {
    die("--timeout 需要是正的秒数");
  }
  return opts;
}

// ---------------------------------------------------------------- 子进程

const children = [];

function pipeLines(stream, tag) {
  let buffered = "";
  stream.setEncoding("utf8");
  stream.on("data", (chunk) => {
    buffered += chunk;
    const lines = buffered.split("\n");
    buffered = lines.pop() ?? "";
    for (const line of lines) say(tag, line.replace(/\r$/, ""));
  });
  stream.on("end", () => {
    if (buffered !== "") say(tag, buffered.replace(/\r$/, ""));
    buffered = "";
  });
}

function start(name, command, args, env) {
  const child = spawn(command, args, {
    cwd: ROOT,
    env: { ...process.env, ...env },
    stdio: ["ignore", "pipe", "pipe"],
    // Unix 上自成进程组，收尾时能整组 SIGTERM；Windows 用 taskkill /T。
    detached: !IS_WINDOWS,
  });
  pipeLines(child.stdout, name);
  pipeLines(child.stderr, name);
  child.on("error", (error) => say("error", `[${name}] 启动失败：${error.message}`));
  children.push(child);
  return child;
}

async function run(name, command, args, env) {
  const child = start(name, command, args, env);
  return await new Promise((resolve) => {
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
}

function killTree(child) {
  if (child === undefined || child.exitCode !== null || child.signalCode !== null) return;
  if (IS_WINDOWS) {
    spawnSync("taskkill", ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore" });
  } else {
    try {
      process.kill(-child.pid, "SIGTERM");
    } catch {
      child.kill("SIGTERM");
    }
  }
}

// ---------------------------------------------------------------- 环境探测

function requiredRustVersion() {
  const manifest = fs.readFileSync(path.join(ROOT, "Cargo.toml"), "utf8");
  return /^rust-version\s*=\s*"([^"]+)"/m.exec(manifest)?.[1] ?? null;
}

function versionAtLeast(actual, required) {
  const a = actual.split(".").map(Number);
  const r = required.split(".").map(Number);
  for (let i = 0; i < 3; i += 1) {
    const [x, y] = [a[i] ?? 0, r[i] ?? 0];
    if (x !== y) return x > y;
  }
  return true;
}

/// 选一个满足 Cargo.toml `rust-version` 的 cargo。优先 `cargo +<version>`（rustup
/// 钉到 MSRV 工具链），否则用 PATH 上的 cargo —— 太旧就报错，而不是让 cargo 自己
/// 抛一句 "rustc 1.x is not supported by the following packages"。
function resolveCargo(required) {
  const prefixes = required === null ? [[]] : [[`+${required}`], []];
  const seen = [];

  for (const prefix of prefixes) {
    const probe = spawnSync("cargo", [...prefix, "--version"], { cwd: ROOT, encoding: "utf8" });
    if (probe.status !== 0) continue;
    const version = /cargo\s+(\d+\.\d+\.\d+)/.exec(probe.stdout ?? "")?.[1];
    if (version === undefined) continue;
    seen.push(`cargo ${[...prefix, "--version"].join(" ")} → ${version}`);
    if (required === null || versionAtLeast(version, required)) {
      return { prefix, version, required };
    }
  }

  die(
    [
      `找不到满足 rust-version = ${required} 的 cargo。试过：`,
      ...seen.map((line) => `  ${line}`),
      "修复：rustup update stable，或 rustup toolchain install " + required,
    ].join("\n"),
  );
}

/// 运行中的 agent 是否已经占着 IPC 端点。Windows 查命名管道是否存在；Unix 按
/// `paths.rs::runtime_dir_for` 的规则找 `agent.sock`。
function endpointExists(profile) {
  if (IS_WINDOWS) return fs.existsSync(`\\\\.\\pipe\\${PIPE_NAME[profile]}`);
  const base =
    process.env.XDG_RUNTIME_DIR ??
    process.env.XDG_CONFIG_HOME ??
    path.join(os.homedir(), ".config");
  return fs.existsSync(path.join(base, APP_DIR[profile], "agent.sock"));
}

function configPath(profile) {
  const base = process.env.XDG_CONFIG_HOME ?? path.join(os.homedir(), ".config");
  return path.join(base, APP_DIR[profile], "config.toml");
}

/// 与 `paths.rs::state_dir` 同一规则：`$XDG_STATE_HOME` 优先，否则 `~/.local/state`。
function stateDir(profile) {
  const base = process.env.XDG_STATE_HOME ?? path.join(os.homedir(), ".local", "state");
  return path.join(base, APP_DIR[profile]);
}

/// Windows 上 agent 每次启动都按 `app_settings.launch_at_login` 重写
/// `HKCU\...\Run\OpenLogiAgent`，值就是自己的可执行文件路径（agent 侧：
/// crates/openlogi-agent/src/autostart/windows.rs）。默认 true，所以跑一次开发态
/// 就会把开机自启指向 target/。
function warnAboutAutostart(profile) {
  if (!IS_WINDOWS) return;
  const file = configPath(profile);
  const text = fs.existsSync(file) ? fs.readFileSync(file, "utf8") : "";
  if (/^\s*launch_at_login\s*=\s*false\s*$/m.test(text)) return;
  say(
    "warn",
    [
      "本次启动会把开机自启（HKCU\\...\\Run\\OpenLogiAgent）改指向当前这个 agent 可执行文件",
      "       —— 开发态就是 target/ 里的那个。",
      `       想保留已安装正式版的自启项，就在 ${file} 里写 launch_at_login = false。`,
    ].join("\n"),
  );
}

// ---------------------------------------------------------------- 主流程

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitForEndpoint(profile, timeoutMs, agent) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (endpointExists(profile)) return true;
    if (agent !== null && (agent.exitCode !== null || agent.signalCode !== null)) return false;
    await delay(100);
  }
  return false;
}

function binaryPath(name, release) {
  const suffix = IS_WINDOWS ? ".exe" : "";
  return path.join(ROOT, "target", release ? "release" : "debug", `${name}${suffix}`);
}

let shuttingDown = false;

function shutdown(reason) {
  if (shuttingDown) return;
  shuttingDown = true;
  say("dev", `收尾（${reason}）…`);
  for (const child of children) killTree(child);
  logStream?.end();
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const profile = opts.profile;
  const buildKind = opts.release ? "release" : "debug";

  if (opts.logFile !== null) {
    fs.mkdirSync(path.dirname(opts.logFile), { recursive: true });
    logStream = fs.createWriteStream(opts.logFile, { flags: "a" });
  }

  say("dev", `OpenLogi dev · profile=${profile} · log=${opts.log} · ${buildKind} 构建`);
  say("dev", `仓库：${ROOT}`);

  const cargo = resolveCargo(requiredRustVersion());
  say("dev", `cargo ${cargo.version}${cargo.prefix.length > 0 ? `（${cargo.prefix[0]}）` : ""}`);

  const agentBin = opts.mock ? "openlogi-agent-mock" : "openlogi-agent";
  const guiBin = "openlogi-desktop";

  if (opts.build) {
    // 一次构建全部三个二进制：`openlogi-agent` 包自带 mock bin，一并出来。
    const args = [
      ...cargo.prefix,
      "build",
      ...(opts.release ? ["--release"] : []),
      "-p",
      "openlogi-desktop",
      "-p",
      "openlogi-overlay",
      "-p",
      "openlogi-agent",
    ];
    say("dev", `构建：cargo ${args.join(" ")}`);
    const started = Date.now();
    const built = await run("build", "cargo", args);
    if (built.code !== 0) {
      shutdown("构建失败");
      process.exit(built.code ?? 1);
    }
    say("dev", `构建完成（${((Date.now() - started) / 1000).toFixed(1)}s）`);
  }

  if (opts.buildOnly) {
    say(
      "dev",
      `${buildKind} 产物在 target/${buildKind}；不启动 GUI、agent 或 overlay。请手动把其中的文件覆盖到生产安装目录。`,
    );
    process.exit(0);
  }

  if (opts.agent) {
    // mock 不跑 autostart reconcile（只有真 agent 的 lifecycle 会写 Run 键）。
    if (!opts.mock) warnAboutAutostart(profile);
    if (endpointExists(profile)) {
      const where = IS_WINDOWS ? `\\\\.\\pipe\\${PIPE_NAME[profile]}` : path.join(APP_DIR[profile], "agent.sock");
      say(
        "warn",
        [
          `IPC 端点 ${where} 已经被占用 —— 多半是已安装的正式版 agent 还在跑。`,
          "       如果这是 dev 管道，说明已有另一个 dev agent 正在运行；不会连接生产 agent。",
          "       请先从托盘 Quit 掉正式版再跑。",
        ].join("\n"),
      );
    }
  }

  const childEnv = {
    OPENLOGI_PROFILE: profile,
    OPENLOGI_LOG: opts.log,
    RUST_BACKTRACE: process.env.RUST_BACKTRACE ?? "1",
  };

  let agent = null;
  if (opts.agent) {
    const bin = binaryPath(agentBin, opts.release);
    if (!fs.existsSync(bin)) {
      shutdown("缺少二进制");
      die(`${bin} 不存在 —— 去掉 --no-build 重新构建`);
    }
    say("dev", `启动服务：${path.relative(ROOT, bin)}`);
    agent = start("agent", bin, [], childEnv);

    say("dev", "等待 agent IPC 就绪 …");
    const ready = await waitForEndpoint(profile, opts.timeoutMs, agent);
    if (ready) {
      say("dev", "agent IPC 已就绪");
    } else if (agent.exitCode !== null || agent.signalCode !== null) {
      shutdown("agent 提前退出");
      say(
        "error",
        [
          "agent 在 IPC 就绪前就退出了 —— 常见原因：",
          "  · 同一 profile 已有另一个 agent 持有单实例锁（看上面的 agent 日志）；",
          `  · 配置或设备初始化失败。agent 自己的日志：${stateDir(profile)}${path.sep}agent.<date>.log`,
        ].join("\n"),
      );
      process.exit(1);
    } else {
      say("warn", `等了 ${opts.timeoutMs / 1000}s 还没看到 IPC 端点，仍然启动 GUI（它会自己重试连接）`);
    }
  }

  const gui = binaryPath(guiBin, opts.release);
  if (!fs.existsSync(gui)) {
    shutdown("缺少二进制");
    die(`${gui} 不存在 —— 去掉 --no-build 重新构建`);
  }
  say("dev", `启动 GUI：${path.relative(ROOT, gui)}`);
  say("dev", "日志实时合并输出；Ctrl-C 结束，GUI 退出也会收掉 agent");
  const guiChild = start("gui", gui, [], childEnv);

  const { code, signal } = await new Promise((resolve) => {
    guiChild.once("exit", (exitCode, exitSignal) => resolve({ code: exitCode, signal: exitSignal }));
  });

  if (code === 0) {
    say("dev", "GUI 已退出");
  } else {
    say("error", `GUI 退出：code=${code} signal=${signal}`);
  }

  shutdown("GUI 退出");
  await delay(300);
  process.exit(code ?? 1);
}

process.on("SIGINT", () => {
  shutdown("Ctrl-C");
  setTimeout(() => process.exit(130), 300);
});
process.on("SIGTERM", () => {
  shutdown("SIGTERM");
  setTimeout(() => process.exit(143), 300);
});

await main();
