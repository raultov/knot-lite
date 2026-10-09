import { UserRepository } from "./userRepository";
import { formatName } from "../util/format";

/** Business operations on users. */
export class UserService {
  constructor(private readonly repository: UserRepository) {}

  displayName(id: number): string {
    const user = this.repository.findById(id);
    return user ? formatName(user) : "unknown";
  }
}
