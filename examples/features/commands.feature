Feature: one-shot commands

  Scenario: the exit code and both streams are asserted, not assumed
    When I run the command "echo hello; echo oops >&2; exit 3"
    # A non-zero exit is not a failure by itself, exactly as an HTTP 500 is not.
    Then the command exit code is 3
    And the command exit code is not 0
    And the command output equals:
      """
      hello
      """
    And the command error output contains "oops"
    And variable "exec_exit_code" should be equal to "3"

  Scenario: a command may carry its own double quotes
    When I run the command "echo "$GREETING, "quoted" world""
    Then the command output equals:
      """
      hello, quoted world
      """

  Scenario: environment and working directory for the rest of the scenario
    Given I run the command "mkdir -p sub"
    And the command environment variable "NAME" is "world"
    And the command working directory is "sub"
    When I run the command "echo "$GREETING, $NAME from $(basename "$PWD")""
    Then the command output equals:
      """
      hello, world from sub
      """

  Scenario: standard input feeds the next command only
    Given the command standard input is:
      """
      pear
      apple
      """
    When I run the command "sort"
    Then the command output has 2 lines
    And the command output matches "^apple\npear"
    When I run the command "cat"
    Then the command output is empty

  Scenario: the argv form passes each argument as written, with no shell
    When I run the command "printf" with arguments:
      | argument   |
      | %s-%s\n    |
      | two words  |
      | $HOME      |
    Then the command output equals:
      """
      two words-$HOME
      """

  Scenario: a value read from the output with a regex
    When I run the command "echo 'created order ord-42 in 3ms'"
    And extract "order (\S+)" from the command output as "orderId"
    Then variable "orderId" should be equal to "ord-42"
    And the command error output is empty

  Scenario: a tighter timeout for this scenario
    Given the command timeout is "2" seconds
    When I run the command "sleep 0.1; echo done"
    Then the command output contains "done"

  Scenario: another instance, selected by name
    Given I use "tools" exec
    When I run the command "echo "bash $BASH_VERSION""
    Then the command output matches "^bash \d+\."
