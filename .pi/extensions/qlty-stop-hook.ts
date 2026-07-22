import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { existsSync } from "node:fs";
import { join } from "node:path";

const HOOK_ENV = "OFFCUT_QLTY_STOP_HOOK_ACTIVE";

function report(ctx: ExtensionContext, message: string, level: "warning" | "error") {
	console.error(message);
	if (ctx.hasUI) {
		try {
			ctx.ui.notify(message, level);
		} catch {
			// The renderer is already torn down on the quit path; stderr carried it.
		}
	}
}

export default function (pi: ExtensionAPI) {
	pi.on("session_shutdown", async (event, ctx) => {
		if (event.reason !== "quit") {
			return;
		}

		if (process.env[HOOK_ENV]) {
			return;
		}

		const rootResult = await pi.exec("git", ["-C", ctx.cwd, "rev-parse", "--show-toplevel"]);
		const root = rootResult.code === 0 ? rootResult.stdout.trim() : ctx.cwd;
		const scriptPath = join(root, ".qlty", "hooks", "qlty-check.py");
		if (!existsSync(scriptPath)) {
			report(ctx, "Qlty stop hook skipped: .qlty/hooks/qlty-check.py was not found", "warning");
			return;
		}

		console.error("Qlty stop hook: running qlty check, this may take a while on a cold plugin cache...");

		const result = await pi.exec("python3", [
			scriptPath,
			"--tool",
			"pi",
			"--cwd",
			ctx.cwd,
		], { timeout: 600_000 });

		if (result.code !== 0) {
			const output = (result.stderr || result.stdout || `qlty hook exited with code ${result.code}`).trim();
			report(ctx, output, "error");
		}
	});
}
