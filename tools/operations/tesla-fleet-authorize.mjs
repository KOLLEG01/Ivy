import { readFile, open, mkdir, chmod } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import {
  createAuthorizationRequest,
  exchangeAuthorizationCode,
} from "../../docs/examples/data-collector/home/tesla-fleet.mjs";

async function writePrivate(file, value) {
  await mkdir(dirname(file), { recursive: true, mode: 0o700 });
  // Do not change permissions on the caller's existing parent directory.
  const handle = await open(file, "wx", 0o600);
  try {
    await handle.writeFile(JSON.stringify(value, null, 2) + "\n");
    await handle.sync();
  } finally {
    await handle.close();
  }
  await chmod(file, 0o600);
}

export async function authorize(args) {
  const [action, ...arguments_] = args;
  const options = {};
  for (let index = 0; index < arguments_.length; index += 2) {
    const key = arguments_[index];
    if (
      !key?.startsWith("--") ||
      !arguments_[index + 1] ||
      Object.hasOwn(options, key)
    )
      throw new Error(
        "Use named file options; see the Tesla Fleet setup guide.",
      );
    options[key] = arguments_[index + 1];
  }
  const required = (key) => {
    if (!options[key]) throw new Error(`Missing ${key}.`);
    return options[key];
  };
  const credentials = JSON.parse(
    await readFile(resolve(required("--client-file")), "utf8"),
  );
  if (action === "begin") {
    const request = createAuthorizationRequest({
      clientId: credentials.clientId,
      redirectUri: required("--redirect-uri"),
      region: options["--region"] ?? "eu",
    });
    await writePrivate(resolve(required("--session-file")), request);
    return { authorizationUrl: request.authorizationUrl };
  }
  if (action === "finish") {
    const sessionFile = resolve(required("--session-file"));
    const request = JSON.parse(await readFile(sessionFile, "utf8"));
    if (request.clientId !== credentials.clientId)
      throw new Error(
        "The client file does not match this authorization session.",
      );
    const callback = await readFile(
      resolve(required("--callback-file")),
      "utf8",
    );
    // Reserve the destination before consuming the one-time authorization code.
    const output = resolve(required("--output"));
    await writePrivate(output, {});
    const tokens = await exchangeAuthorizationCode(
      request,
      callback,
      credentials.clientSecret,
    );
    const handle = await open(output, "r+");
    try {
      await handle.truncate(0);
      await handle.writeFile(
        JSON.stringify(
          {
            tesla_client_id: request.clientId,
            tesla_refresh_token: tokens.refreshToken,
          },
          null,
          2,
        ) + "\n",
      );
      await handle.sync();
    } finally {
      await handle.close();
    }
    return {
      output,
      message:
        "Add these secrets to the collector's protected settings. Tokens were not printed.",
    };
  }
  throw new Error("Use begin or finish; see the Tesla Fleet setup guide.");
}

if (
  process.argv[1] &&
  pathToFileURL(resolve(process.argv[1])).href === import.meta.url
) {
  try {
    process.stdout.write(
      JSON.stringify(await authorize(process.argv.slice(2))) + "\n",
    );
  } catch (error) {
    const message =
      error.code === "ENOENT"
        ? "A required private file was not found."
        : error.code === "EEXIST"
          ? "The destination already exists; choose a new private file."
          : error instanceof SyntaxError
            ? "A private configuration file contains invalid JSON."
            : error.code &&
                ![
                  "configuration_invalid",
                  "authentication_required",
                  "interaction_required",
                  "provider_unavailable",
                ].includes(error.code)
              ? "The private authorization files could not be accessed."
              : error.message;
    process.stderr.write(message + "\n");
    process.exitCode = 1;
  }
}
