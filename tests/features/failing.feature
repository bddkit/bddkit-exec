Feature: each scenario fails in its own way, and says so

  Scenario: a timeout kills the command and fails the step
    Given the command timeout is "0.3" seconds
    When I run the command "echo started; sleep 5"

  Scenario: a stream past max_output_bytes fails the next assertion on it
    Given I use "tiny" exec
    When I run the command "seq 1 100"
    Then the command output contains "1"

  Scenario: a NUL byte cannot reach exec
    When I run the command "echo <<null>>"

  Scenario: an eventual assertion on a process gives up with the last observation
    Given I start the "quiet" process running "echo only this; sleep 30"
    And I expect the next assertion to pass within "1" seconds
    Then the "quiet" process output contains "never printed"

  Scenario: a wrong exit code carries the command and both streams
    When I run the command "echo to-stdout; echo to-stderr >&2; exit 4"
    Then the command exit code is 0

  Scenario: a process that was never started is named with the ones that were
    Given I start the "one" process running "sleep 30"
    Then the "two" process output is empty
