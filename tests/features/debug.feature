Feature: debug traces go to stderr

  Scenario: the command line and the exit code are traced
    Given I am in debug mode
    When I run the command "echo traced; exit 2"
    Then the command exit code is 2
