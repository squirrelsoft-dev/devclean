import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { existsSync } from "node:fs";
import { join } from "node:path";

const HOOK_ENV = "OFFCUT_QLTY_STOP_HOOK_ACTIVE";

export default function (pi: ExtensionAPI) {
	pi.on("session_shutdown", async (event, ctx) => {
		if (process.env[HOOK_ENV]) {
			return;
		}

		const rootResult = await pi.exec("git", ["-C", ctx.cwd, "rev-parse", "--show-toplevel"]);
		const root = rootResult.code === 0 ? rootResult.stdout.trim() : ctx.cwd;
		const scriptPath = join(root, ".qlty", "hooks", "qlty-check.py");
		if (!existsSync(scriptPath)) {
			if (ctx.hasUI) {
				ctx.ui.notify("Qlty stop hook skipped: .qlty/hooks/qlty-check.py was not found", "warning");
			}
			return;
		}

		const result = await pi.exec("python3", [
			scriptPath,
			"--tool",
			"pi",
			"--cwd",
			ctx.cwd,
			"--event",
			event.reason,
		], { timeout: 600_000 });

		if (result.code !== 0) {
			const output = (result.stderr || result.stdout || `qlty hook exited with code ${result.code}`).trim();
			if (ctx.hasUI) {
				ctx.ui.notify(output, "error");
			} else {
				console.error(output);
			}
		}
	});
}
