Feature: cmd.exe and PowerShell on Windows

  Scenario: cmd.exe: an exit code and an output ending in CRLF
    When I run the command "echo hello& exit /b 3"
    Then the command exit code is 3
    # One trailing line ending is ignored, \r\n as much as \n.
    And the command output equals:
      """
      hello
      """

  Scenario: cmd.exe: the scenario's environment
    Given the command environment variable "NAME" is "world"
    When I run the command "echo hello %NAME%"
    Then the command output contains "hello world"

  Scenario: PowerShell, selected by name
    Given I use "ps" exec
    When I run the command "Write-Output (6 * 7)"
    Then the command output equals:
      """
      42
      """

  Scenario: a background process is stopped with its whole tree
    Given I start the "pinger" process running "ping -n 30 127.0.0.1"
    And I expect the next assertion to pass within "10" seconds
    And the "pinger" process output contains "127.0.0.1"
    When I stop the "pinger" process
    Then the "pinger" process output contains "127.0.0.1"

  Scenario: the argv form needs no shell at all
    When I run the command "hostname" with arguments:
      | argument |
    Then the command exit code is 0
