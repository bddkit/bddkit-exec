Feature: a suite that declares no exec instance still runs commands

  # The implicit instance uses the platform's default shell — sh, or cmd on
  # Windows — so this one command is written to mean the same in both.
  Scenario: the implicit instance runs a command
    When I run the command "echo hello"
    Then the command exit code is 0
    And the command output equals:
      """
      hello
      """
