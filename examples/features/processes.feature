Feature: background processes keep streaming while the scenario goes on

  Scenario: an eventual assertion re-reads the stream until it passes
    Given I start the "ticker" process running "for i in 1 2 3; do echo tick $i; sleep 0.3; done"
    Then the "ticker" process should be running
    And I expect the next assertion to pass within "5" seconds
    And the "ticker" process output contains "tick 3"
    And I expect the next assertion to pass within "5" seconds
    And the "ticker" process should have exited with code 0
    And the "ticker" process output has 3 lines

  Scenario: two log tails, one event, the positive before the negative
    # The logs are new files in this feature file's workspace, so reading them
    # from the top (`-n +1`) is exact. Against a real, long-lived log use
    # `-n0` and see the README on awaiting a probe line first.
    Given I run the command "touch access.log error.log"
    And I start the "access" process running "tail -n +1 -F access.log"
    And I start the "error" process running "tail -n +1 -F error.log"
    When I run the command "echo 'GET /users?legacy=1 200' >> access.log; echo 'WARN legacy parameter used' >> error.log"
    Then I expect the next assertion to pass within "5" seconds
    And the "access" process output contains "GET /users?legacy=1 200"
    And I expect the next assertion to pass within "5" seconds
    And the "error" process output matches "WARN .*legacy parameter"
    # On a live stream a negative proves something only after a positive
    # about the same event; against an empty buffer it passes instantly.
    And the "error" process output does not contain "ERROR"

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
