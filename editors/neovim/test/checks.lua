-- Runs inside `nvim --clean --headless` (see run.sh), with no kotlin-analyzer
-- on the PATH and KOTLIN_ANALYZER_DOWNLOADS at a local release.
--
--   checks.lua downloads <workspace>   setup() downloads the release, then
--                                      diagnostics, hover, definition, reload
--   checks.lua tampered                a release whose checksum is wrong is
--                                      refused and leaves no binary

local PATIENCE_MILLISECONDS = 60000

local scenario, workspace = arg[1], arg[2]
local failures = 0
local messages = {}

vim.notify = function(message, level)
  table.insert(messages, { message = message, level = level })
end

local function check(name, body)
  local ok, failure = pcall(body)
  if ok then
    io.stdout:write("  ✔ " .. name .. "\n")
  else
    failures = failures + 1
    io.stdout:write("  ✘ " .. name .. ": " .. tostring(failure) .. "\n")
  end
end

local function wait_for(what, probe)
  local value
  local found = vim.wait(PATIENCE_MILLISECONDS, function()
    value = probe()
    return value ~= nil
  end, 100)
  assert(found, "timed out waiting for " .. what)
  return value
end

local function request(buffer, method, params)
  local client = vim.lsp.get_clients({ bufnr = buffer, name = "kotlin_analyzer" })[1]
  assert(client, "a client attached to the buffer")
  local response = client:request_sync(method, params, PATIENCE_MILLISECONDS, buffer)
  assert(response and response.err == nil, method .. " failed: " .. vim.inspect(response))
  return response.result
end

local function position_of(buffer, text)
  for line_number, line in ipairs(vim.api.nvim_buf_get_lines(buffer, 0, -1, false)) do
    local column = line:find(text, 1, true)
    if column then
      return { line = line_number - 1, character = column }
    end
  end
  error(text .. " isn't in the buffer")
end

local function downloads()
  local sources = vim.fs.joinpath(workspace, "app/src/main/kotlin/app")
  local release = require("kotlin_analyzer.release")
  local version = require("kotlin_analyzer.version")

  check("setup() downloads the release and starts it", function()
    vim.cmd.edit(vim.fs.joinpath(sources, "Broken.kt"))
    require("kotlin_analyzer").setup()
    local server = wait_for("the server to start", function()
      return #vim.lsp.get_clients({ name = "kotlin_analyzer" }) > 0 and require("kotlin_analyzer").server or nil
    end)
    assert(server.source == "release", vim.inspect(server))
    assert(server.command[1] == release.binary(version), vim.inspect(server))
    assert(vim.uv.fs_stat(release.binary(version)), "the binary is installed")
  end)

  check("the server opens the Gradle build's root, not the module", function()
    local client = vim.lsp.get_clients({ name = "kotlin_analyzer" })[1]
    assert(client.root_dir == vim.uv.fs_realpath(workspace) or client.root_dir == workspace, client.root_dir)
  end)

  check("syntax errors come back as diagnostics", function()
    local buffer = vim.api.nvim_get_current_buf()
    local diagnostics = wait_for("diagnostics", function()
      local found = vim.diagnostic.get(buffer)
      return #found > 0 and found or nil
    end)
    assert(diagnostics[1].severity == vim.diagnostic.severity.ERROR, vim.inspect(diagnostics))
  end)

  check("hover and definition", function()
    vim.cmd.edit(vim.fs.joinpath(sources, "Use.kt"))
    local buffer = vim.api.nvim_get_current_buf()
    wait_for("the client to attach", function()
      return #vim.lsp.get_clients({ bufnr = buffer, name = "kotlin_analyzer" }) > 0 or nil
    end)
    local position = position_of(buffer, "Box(")
    local params = { textDocument = vim.lsp.util.make_text_document_params(buffer), position = position }
    local hover = wait_for("a hover on Box", function()
      local result = request(buffer, "textDocument/hover", params)
      local text = result and vim.inspect(result.contents) or ""
      return text:find("class Box", 1, true) and text or nil
    end)
    assert(hover)
    local definition = request(buffer, "textDocument/definition", params)
    local locations = vim.islist(definition) and definition or { definition }
    assert(#locations == 1, vim.inspect(definition))
    local uri = locations[1].uri or locations[1].targetUri
    assert(vim.endswith(uri, "/Box.kt"), uri)
  end)

  check(":KotlinGradle runs Gradle for the file's project or a parent", function()
    vim.cmd.edit(vim.fs.joinpath(sources, "Box.kt"))
    local completions = vim.fn.getcompletion("KotlinGradle test ", "cmdline")
    assert(vim.deep_equal(completions, { ":app", ":" }), vim.inspect(completions))
    local call_file = vim.env.GRADLE_CALL
    local cases = {
      { "KotlinGradle build", { "build", "-x", "test" } },
      { "KotlinGradle clean", { "clean" } },
      { "KotlinGradle test", { "-p", "app", "test", "--rerun-tasks" } },
      { "KotlinGradle test :", { "test", "--rerun-tasks" } },
    }
    for _, case in ipairs(cases) do
      vim.fn.delete(call_file)
      local source_window = vim.api.nvim_get_current_win()
      vim.cmd(case[1])
      local lines = wait_for("gradle from " .. case[1], function()
        local written = vim.uv.fs_stat(call_file) and vim.fn.readfile(call_file) or {}
        return #written > 0 and written or nil
      end)
      vim.cmd("stopinsert")
      vim.cmd.close()
      vim.api.nvim_set_current_win(source_window)
      assert(vim.uv.fs_realpath(lines[1]) == vim.uv.fs_realpath(workspace), lines[1])
      local arguments = vim.list_slice(lines, 2)
      assert(vim.deep_equal(arguments, case[2]), case[1] .. ": " .. vim.inspect(arguments))
    end
  end)

  check("a test's code lens runs it by Gradle", function()
    vim.cmd.edit(vim.fs.joinpath(sources, "BoxTest.kt"))
    local buffer = vim.api.nvim_get_current_buf()
    wait_for("the client to attach", function()
      return #vim.lsp.get_clients({ bufnr = buffer, name = "kotlin_analyzer" }) > 0 or nil
    end)
    local lens = wait_for("a Run test lens", function()
      local lenses = request(buffer, "textDocument/codeLens", {
        textDocument = vim.lsp.util.make_text_document_params(buffer),
      }) or {}
      for _, candidate in ipairs(lenses) do
        if candidate.command and candidate.command.title == "▶ Run test" then
          return candidate
        end
      end
    end)
    local call_file = vim.env.GRADLE_CALL
    vim.fn.delete(call_file)
    local source_window = vim.api.nvim_get_current_win()
    local client = vim.lsp.get_clients({ bufnr = buffer, name = "kotlin_analyzer" })[1]
    client:exec_cmd(lens.command, { bufnr = buffer })
    local lines = wait_for("gradle from the lens", function()
      local written = vim.uv.fs_stat(call_file) and vim.fn.readfile(call_file) or {}
      return #written > 0 and written or nil
    end)
    vim.cmd("stopinsert")
    vim.cmd.close()
    vim.api.nvim_set_current_win(source_window)
    local arguments = vim.list_slice(lines, 2)
    local expected = { "-p", "app", "test", "--tests", "app.BoxTest.holds", "--rerun" }
    assert(vim.deep_equal(arguments, expected), vim.inspect(arguments))
  end)

  check(":KotlinAnalyzerReload runs the server's reload", function()
    local errors_before = #vim.tbl_filter(function(message)
      return message.level == vim.log.levels.ERROR
    end, messages)
    vim.cmd("KotlinAnalyzerReload")
    vim.wait(2000)
    local errors_after = #vim.tbl_filter(function(message)
      return message.level == vim.log.levels.ERROR
    end, messages)
    assert(errors_after == errors_before, vim.inspect(messages))
  end)
end

local function tampered()
  local release = require("kotlin_analyzer.release")
  local version = require("kotlin_analyzer.version")
  check("a release with the wrong checksum is refused", function()
    require("kotlin_analyzer").setup()
    local refusal = wait_for("the refusal", function()
      for _, message in ipairs(messages) do
        if message.level == vim.log.levels.ERROR then
          return message.message
        end
      end
    end)
    assert(refusal:find("SHA-256", 1, true), refusal)
    assert(vim.uv.fs_stat(release.binary(version)) == nil, "no binary is left behind")
    assert(#vim.lsp.get_clients({ name = "kotlin_analyzer" }) == 0, "nothing started")
  end)
end

if scenario == "downloads" then
  downloads()
elseif scenario == "tampered" then
  tampered()
else
  error("unknown scenario " .. tostring(scenario))
end
for _, client in ipairs(vim.lsp.get_clients()) do
  client:stop()
end
vim.wait(1000)
os.exit(failures == 0 and 0 or 1)
