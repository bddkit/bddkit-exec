Feature: stopping a background process on Windows

  # `I stop` sends CTRL_BREAK. Git Bash does not turn it into SIGINT, so this
  # uses a program that listens for Break itself: PowerShell with a
  # Console.CancelKeyPress hook.
  Scenario: a process that handles Break leaves a final buffer and a readable exit
    Given I use "ps" exec
    And I start the "server" process running "Add-Type -TypeDefinition 'public static class B { public static volatile bool Stop; public static void Hook() { System.Console.CancelKeyPress += (s, e) => { e.Cancel = true; Stop = true; }; } }'; [B]::Hook(); 'ready'; while (-not [B]::Stop) { Start-Sleep -Milliseconds 100 }; 'bye'"
    And I expect the next assertion to pass within "15" seconds
    And the "server" process output contains "ready"
    When I stop the "server" process
    Then the "server" process should have exited with code 0
    # equals: with a two-line doc string does not fit PowerShell output, whose
    # lines end in \r\n and only one trailing line ending is ignored.
    And the "server" process output has 2 lines
    And the "server" process output contains "bye"
