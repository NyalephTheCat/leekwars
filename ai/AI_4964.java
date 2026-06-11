import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_4964 extends AI {
public AI_4964() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
session.rebindAll(this);
var u_a = session.getVariable("a");
var u_m = session.getVariable("m");
var u_t = session.getVariable("t");
return ops(putv4(u_t.get(), 0l, 1l, null), 1);
}
protected String getAIString() { return "<snippet 4964>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 4964>", };}

protected int[] getErrorFilesID() { return new int[] {4964, };}

}
