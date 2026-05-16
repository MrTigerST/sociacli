-- ping/main.lua — sociacli demo plugin
--
-- Demonstrates the SDK surface. Two entrypoints; both optional.
--   run(args)           called by `sociacli plugin run ping [--args JSON]`
--   on_message(ctx,a)   called when a trusted friend invokes this plugin
--                       on us via `sociacli plugin send <us> ping ...`

function run(args)
  local who   = sociacli.me or "(local)"
  local text  = (args and args.text) or "pong"
  local plug  = sociacli.plugin
  local stamp = sociacli.now()

  -- identity + manifest
  sociacli.log("info", string.format(
    "[ping] plugin=%s v%s by %s — running as %s @ %d",
    plug.id, plug.version, plug.author, who, stamp))

  -- KV store
  local count = (sociacli.store_get("invocations") or 0) + 1
  sociacli.store_set("invocations", count)
  sociacli.log("info", string.format("[ping] invocation #%d", count))

  -- encoding helpers
  local payload = sociacli.json_encode({ text = text, at = stamp, id = sociacli.uuid() })
  local digest  = sociacli.sha256(payload)
  local b64     = sociacli.base64_encode(payload)
  sociacli.log("debug", "sha256:" .. digest)
  sociacli.log("debug", "base64:" .. b64)

  -- sandboxed file write
  sociacli.write_file("last.txt", payload)
  local readback = sociacli.read_file("last.txt")
  assert(readback == payload, "round-trip mismatch")

  sociacli.notify("Ping #" .. tostring(count), text)
end

function on_message(ctx, args)
  local text = (args and args.text) or "(no text)"
  sociacli.log("info", string.format("[ping] %s says %s", ctx.from, text))
  sociacli.notify("Ping from " .. ctx.from, text)

  -- Echo back if we know how to reach them.
  -- sociacli.send(ctx.from, { text = "pong: " .. text })
end
