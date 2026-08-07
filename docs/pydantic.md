# Pydantic Integration

Protobuf message classes work with [Pydantic](https://docs.pydantic.dev/) out of the box.
Messages can be used as field types in models, validated directly with a `TypeAdapter`, and used
as request and response types in frameworks built on Pydantic such as FastAPI.

Validation and serialization follow the [Protobuf JSON format](./serialization.md#json):
field names use their JSON (lowerCamelCase) names, 64-bit integers are strings, `bytes` are base64, and enums are serialized by name.
Unknown fields are ignored during validation, matching Protobuf's schema evolution semantics.

The examples below use the `User` message from the [quickstart](./getting-started/quickstart.md):

```proto
message User {
  string first_name = 1;
  string last_name = 2;
  bool active = 3;
  User manager = 4;
  repeated string locations = 5;
  map<string, string> projects = 6;
}
```

## Messages as model fields

A message class can be declared as a field of any Pydantic model as normal:

```python
from pydantic import BaseModel

from gen.user_pb import User


class Account(BaseModel):
    id: int
    user: User
```

The field validates and serializes with the rest of the model.
`model_dump_json` renders the message in Protobuf JSON form, and validation accepts either a message instance or Protobuf JSON data:

```python
account = Account(id=1, user=User(first_name="Homer", active=True))
account.model_dump_json()
# '{"id":1,"user":{"firstName":"Homer","active":true}}'

Account.model_validate_json('{"id": 1, "user": {"firstName": "Homer"}}')
Account.model_validate({"id": 1, "user": {"firstName": "Homer"}})
```

Values that don't conform to the schema raise Pydantic's `ValidationError`:

```python
Account.model_validate({"id": 1, "user": {"active": "bear"}})
# ValidationError: 1 validation error for Account
```

In Python mode (`model_dump`), the message is returned as-is rather than converted to a `dict`:

```python
account.model_dump()
# {'id': 1, 'user': User(first_name='Homer', active=True)}
```

## Using a message like a model

A `TypeAdapter` applies Pydantic's model interface, including validation, serialization, and JSON schema generation,
to a message class directly:

```python
from pydantic import TypeAdapter

from gen.user_pb import User

ta = TypeAdapter(User)

user = ta.validate_json('{"firstName": "Homer", "active": true}')
# User(first_name='Homer', active=True)

ta.dump_json(user)
# b'{"firstName":"Homer","active":true}'

ta.json_schema()
# {'properties': {'firstName': {'title': 'first_name', 'type': 'string'}, ...}
```

For plain serialization, the message's own [`to_json` and `from_json`](./serialization.md) methods are simpler and don't require Pydantic.
A `TypeAdapter` is useful when a library expects the Pydantic interface, or to generate a JSON schema for a message.

Generated JSON schemas match the Protobuf JSON format, including string-encoded 64-bit integers, base64 `bytes`, enum value names, and the custom forms of well-known types like `Timestamp` and `Duration`.
Comments from the `.proto` file are included as schema descriptions.

## FastAPI

FastAPI validates requests and responses with Pydantic, so messages can be used as body parameters and response types:

```python
from typing import Annotated

from fastapi import Body, FastAPI

from gen.user_pb import User

app = FastAPI()


@app.post("/users")
async def create_user(user: Annotated[User, Body()]) -> User:
    user.active = True
    return user
```

The request body is parsed from Protobuf JSON, invalid input is rejected with a `422` response, and the returned message is serialized back to Protobuf JSON.
The message's JSON schema appears in the generated OpenAPI document and renders in FastAPI's interactive docs page.

Give it a try with `fastapi run` and visiting the `/docs` page in a browser.

!!! note
    The `Annotated[User, Body()]` annotation is required for request parameters.
    FastAPI only infers a body location for `BaseModel` subclasses; other types default to query parameters.
    Response types need no annotation.
