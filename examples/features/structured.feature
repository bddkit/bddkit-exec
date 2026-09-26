Feature: structured output: JSON, CSV and TSV

  Scenario: JSON, loosely and exactly
    When I run the command "printf '{"id": 7, "tags": ["a", "b"], "owner": {"name": "ann"}}'"
    # contains: an object subset, arrays in any order, extras allowed
    Then the command output contains JSON:
      """
      {"tags": ["b"], "owner": {"name": "ann"}}
      """
    And the command output equals JSON:
      """
      {"owner": {"name": "ann"}, "id": 7, "tags": ["a", "b"]}
      """
    And the command output does not contain JSON:
      """
      {"id": 8}
      """
    When extract "owner.name" from the command output as JSON as "owner"
    Then variable "owner" should be equal to "ann"

  Scenario: CSV follows RFC 4180 quoting
    Given the command standard input is:
      """
      id,name,note
      1,ann,"likes ""tea"", a lot"
      2,bob,plain
      """
    When I run the command "cat"
    # contains: the table names the columns it cares about, in any order
    Then the command output as CSV contains:
      | note               | name |
      | likes "tea", a lot | ann  |
    And the command output as CSV equals:
      | id | name | note               |
      | 1  | ann  | likes "tea", a lot |
      | 2  | bob  | plain              |

  Scenario: TSV has no quoting at all
    When I run the command "printf 'id\tname\n1\t"ann"\n'"
    Then the command output as TSV equals:
      | id | name  |
      | 1  | "ann" |

  Scenario: one row found in a TSV table
    When I run the command "printf 'id\tname\trole\n1\tann\tadmin\n2\tbob\tuser\n'"
    # contains: the table names only the columns it cares about, in any
    # order; the other columns and rows of the output are not checked
    Then the command output as TSV contains:
      | role | name |
      | user | bob  |
