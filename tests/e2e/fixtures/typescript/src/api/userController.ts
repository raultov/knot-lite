import { UserService } from "../services/userService";
import { formatName } from "../util/format";
import type { User } from "../models/user";

/** HTTP-like entry points for users. */
export class UserController {
  constructor(private readonly service: UserService) {}

  show(id: number): string {
    return this.service.displayName(id);
  }

  greet(user: User): string {
    return `Hello, ${formatName(user)}`;
  }
}
