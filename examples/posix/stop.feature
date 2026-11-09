Feature: stopping a background process that traps SIGTERM and SIGINT

  # POSIX only. On Windows `I stop` sends CTRL_BREAK, which Git Bash does not
  # turn into SIGINT, so a `trap` never runs there; see
  # examples/windows/processes.feature for the Windows counterpart.

  Scenario: a stopped process leaves a final buffer and a readable exit
    Given I start the "server" process running "trap 'echo bye; exit 0' TERM INT; echo ready; while :; do sleep 0.1; done"
    And I expect the next assertion to pass within "5" seconds
    And the "server" process output contains "ready"
    When I stop the "server" process
    Then the "server" process should have exited with code 0
    And the "server" process output equals:
      """
      ready
      bye
      """
    When extract "(\w+)\s*$" from the "server" process output as "lastWord"
    Then variable "lastWord" should be equal to "bye"
