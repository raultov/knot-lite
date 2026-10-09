import type { User } from "../models/user";

/** Full display name of a user. */
export function formatName(user: User): string {
  return `${user.firstName} ${user.lastName}`;
}
