Feature: nothing a scenario starts outlives it

  # PIDDIR comes from the test harness's environment. The pipeline member
  # records its own pid: killing only the leader would orphan it.
  Scenario: a background pipeline is started and left running
    Given I start the "first" process running "echo $$ > "$PIDDIR/leader"; sh -c 'echo $$ > "$PIDDIR/member"; exec sleep 300' | cat; echo never"
    And I start the "ready" process running "while [ ! -s "$PIDDIR/member" ]; do sleep 0.05; done; echo ready"
    And I expect the next assertion to pass within "5" seconds
    Then the "ready" process output contains "ready"

  Scenario: the reset at the boundary killed the whole group
    # Both pid files must exist, or a missing one would pass this vacuously.
    When I run the command "test -s "$PIDDIR/leader" && test -s "$PIDDIR/member" && ! kill -0 "$(cat "$PIDDIR/leader")" 2>/dev/null && ! kill -0 "$(cat "$PIDDIR/member")" 2>/dev/null"
    Then the command exit code is 0
    Given I start the "last" process running "echo $$ > "$PIDDIR/last"; echo up; exec sleep 300"
    And I expect the next assertion to pass within "5" seconds
    Then the "last" process output contains "up"
