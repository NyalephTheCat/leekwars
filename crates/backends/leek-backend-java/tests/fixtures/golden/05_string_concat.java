import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_1 extends AI {
public AI_1() throws LeekRunException {
super(3, 4);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
Object u_name = ops("world", 1);
Object u_greeting = ops((String) add((String) add("hello, ", u_name), "!"), 3);
return u_greeting;
}
protected String getAIString() { return "05_string_concat.leek";}
protected String[] getErrorFiles() { return new String[] {"05_string_concat.leek", };}

protected int[] getErrorFilesID() { return new int[] {1, };}

}
