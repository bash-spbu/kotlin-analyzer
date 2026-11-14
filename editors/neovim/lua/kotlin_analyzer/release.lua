-- The server's GitHub releases: which asset fits this machine, and
-- installing it with its checksum verified (curl, gzip and shasum/sha256sum).

local release = {}

release.DOWNLOADS = "https://github.com/bash-spbu/kotlin-analyzer/releases/download"

--- This machine's Rust target triple, or nil and why there's no release.
---@return string|nil triple
---@return string|nil reason
function release.target()
  local uname = vim.uv.os_uname()
  local systems = { Darwin = "apple-darwin", Linux = "unknown-linux-gnu" }
  local machines = { arm64 = "aarch64", aarch64 = "aarch64", x86_64 = "x86_64", amd64 = "x86_64" }
  local system = systems[uname.sysname]
  local machine = machines[uname.machine]
  if system == nil then
    return nil, "no kotlin-analyzer release for " .. uname.sysname
  end
  if machine == nil then
    return nil, "no kotlin-analyzer release for " .. uname.machine
  end
  return machine .. "-" .. system, nil
end

---@param triple string
---@return string
function release.asset(triple)
  return "kotlin-analyzer-" .. triple .. ".gz"
end

--- Where release `version` is downloaded from (KOTLIN_ANALYZER_DOWNLOADS
--- overrides the base, for tests).
---@param version string
---@return string
function release.url(version)
  local base = vim.env.KOTLIN_ANALYZER_DOWNLOADS or release.DOWNLOADS
  return base .. "/v" .. version
end

--- Where a downloaded release lives: one directory per version.
---@param version string
---@return string
function release.binary(version)
  return vim.fs.joinpath(vim.fn.stdpath("data"), "kotlin-analyzer", version, "kotlin-analyzer")
end

--- Runs `command`, resuming the calling coroutine with its result.
local function run(command)
  local thread = coroutine.running()
  vim.system(command, { text = true }, function(result)
    vim.schedule(function()
      coroutine.resume(thread, result)
    end)
  end)
  return coroutine.yield()
end

local function sha256(file)
  local tool = vim.fn.executable("sha256sum") == 1 and { "sha256sum", file } or { "shasum", "-a", "256", file }
  local result = run(tool)
  if result.code ~= 0 then
    return nil
  end
  return vim.split(result.stdout, "%s")[1]
end

local function download(url, file)
  local result = run({ "curl", "--fail", "--silent", "--show-error", "--location", "--output", file, url })
  if result.code ~= 0 then
    error(("downloading %s: %s"):format(url, vim.trim(result.stderr)), 0)
  end
end

--- Installs release `version` at `release.binary(version)`. Runs in a
--- coroutine; `on_done(error)` gets nil on success. A failed install never
--- leaves a binary behind.
---@param version string
---@param on_done fun(error: string|nil)
function release.install(version, on_done)
  coroutine.wrap(function()
    local ok, failure = pcall(function()
      local triple, reason = release.target()
      if triple == nil then
        error(reason, 0)
      end
      local asset = release.asset(triple)
      local work = vim.fn.tempname()
      vim.fn.mkdir(work, "p")
      local packed = vim.fs.joinpath(work, asset)
      download(release.url(version) .. "/" .. asset, packed)
      download(release.url(version) .. "/" .. asset .. ".sha256", packed .. ".sha256")
      local expected = vim.split(vim.trim(table.concat(vim.fn.readfile(packed .. ".sha256"), "\n")), "%s")[1]
      local actual = sha256(packed)
      if actual == nil or actual:lower() ~= expected:lower() then
        vim.fn.delete(work, "rf")
        error(("%s has SHA-256 %s, the release says %s"):format(asset, tostring(actual), expected), 0)
      end
      local unpacked = run({ "gzip", "-d", "-f", packed })
      if unpacked.code ~= 0 then
        error("unpacking " .. asset .. ": " .. vim.trim(unpacked.stderr), 0)
      end
      local destination = release.binary(version)
      vim.fn.mkdir(vim.fs.dirname(destination), "p")
      local binary = packed:gsub("%.gz$", "")
      assert(vim.uv.fs_chmod(binary, tonumber("755", 8)))
      assert(vim.uv.fs_rename(binary, destination))
      vim.fn.delete(work, "rf")
    end)
    on_done(not ok and tostring(failure) or nil)
  end)()
end

return release
